#[cfg(feature = "devtools")]
use blitz_traits::devtools::{
    AttributeChanged, CharacterDataChanged, MutationRecord, NodeInserted, NodeRemoved,
};
use blitz_traits::node_id::NodeId;
use std::collections::HashSet;
use std::mem;
use std::ops::{Deref, DerefMut, Range};

use crate::layout::damage::{ALL_DAMAGE, CONSTRUCT_BOX, ONLY_RELAYOUT};
use crate::net::{ResourceHandler, StylesheetHandler};
use crate::node::{CanvasData, NodeFlags, ShadowRootMode, SpecialElementData, TextInputData};
use crate::stylo::DeviceChanges;
use crate::util::ImageType;
use crate::{
    Attribute, BaseDocument, Document, ElementData, LocalName, Node, NodeData, QualName,
    local_name, ns, qual_name,
};
use blitz_traits::shell::Viewport;
use selectors::matching::ElementSelectorFlags;
use style::Atom;
use style::invalidation::element::restyle_hints::RestyleHint;
use style::stylesheets::OriginSet;
use style_dom::ElementState;
use thin_vec::ThinVec;

const TEMPLATE_CONTENTS_FRAGMENT_NAME: &str = "#document-fragment";

/// Whether inserting a child must recascade the parent's whole subtree: its own `:empty`, a
/// `:last-child`-style or `sibling-index()` dependency on every child, or a flat tree that
/// slotting reshapes. Anything else goes through `restyle_for_child_list_change`.
fn insertion_restyles_whole_parent(parent: &Node) -> bool {
    use selectors::matching::ElementSelectorFlags;

    if !parent.is_element()
        || parent.is_slot_element()
        || parent.attached_shadow_root_id().is_some()
    {
        return true;
    }
    // `dir=auto` and `<bdi>` take their direction from their descendants' text, and no selector
    // flag covers that.
    let direction_from_content = parent
        .element_data()
        .is_some_and(|data| data.name.local == local_name!("bdi"))
        || parent
            .attr(local_name!("dir"))
            .is_some_and(|dir| dir.eq_ignore_ascii_case("auto"));
    direction_from_content
        || parent.selector_flags().get().intersects(
            ElementSelectorFlags::HAS_EMPTY_SELECTOR
                | ElementSelectorFlags::HAS_SLOW_SELECTOR
                | ElementSelectorFlags::MAY_HAVE_TREE_COUNTING_FUNCTION,
        )
}

macro_rules! tag_and_attr {
    ($tag:tt, $attr:tt) => {
        (&local_name!($tag), &local_name!($attr))
    };
}

/// Buffer a mutation record for the document's devtools provider (if any).
/// The record is only built if there is a provider.
macro_rules! devtools {
    ($mutr:expr, $record:expr) => {
        #[cfg(feature = "devtools")]
        if $mutr.doc.devtools_provider.is_some() {
            $mutr.devtools_records.push($record);
        }
    };
}

/// Whether `node_id` is a `<template>`, its contents fragment, or inside one. Template contents
/// are never flagged in-document, so the walk ends at the first in-document node: O(1) for the
/// usual insertion into a live tree.
fn is_template_inert_context(doc: &BaseDocument, node_id: NodeId) -> bool {
    let mut current = Some(node_id);
    while let Some(id) = current {
        let Some(node) = doc.get_node(id) else {
            return false;
        };
        let in_template_context = node.element_data().is_some_and(|element| {
            *element.namespace() == ns!(html)
                && (element.name.local == local_name!("template")
                    || &*element.name.local == TEMPLATE_CONTENTS_FRAGMENT_NAME)
        });
        if in_template_context {
            return true;
        }
        if node.flags.is_in_document() {
            return false;
        }
        current = node.parent_id();
    }
    false
}

#[derive(Debug, Clone)]
pub enum AppendTextErr {
    /// The node is not a text node
    NotTextNode,
}

/// Operations that happen almost immediately, but are deferred within a
/// function for borrow-checker reasons.
enum SpecialOp {
    LoadImage(NodeId),
    LoadIframe(NodeId),
    LoadStylesheet(NodeId),
    UnloadStylesheet(NodeId),
    LoadCustomPaintSource(NodeId),
    ProcessButtonInput(NodeId),
    UnloadSubDocument(NodeId),
    #[cfg(feature = "custom-widget")]
    UnloadCustomWidget(NodeId),
}

pub struct DocumentMutator<'doc> {
    /// Document is public as an escape hatch, but users of this API should ideally avoid using it
    /// and prefer exposing additional functionality in DocumentMutator.
    pub doc: &'doc mut BaseDocument,

    eager_op_queue: Vec<SpecialOp>,

    // Tracked nodes for deferred processing when mutations have completed
    title_node: Option<NodeId>,
    style_nodes: HashSet<NodeId>,
    form_nodes: HashSet<NodeId>,

    /// Whether an element/attribute that affect animation status has been seen
    recompute_is_animating: bool,

    /// Whether any mutation that affects rendered output has been performed
    mutations_occurred: bool,

    /// The (latest) node which has been mounted in and had autofocus=true, if any
    #[cfg(feature = "autofocus")]
    node_to_autofocus: Option<NodeId>,

    /// Mutations to report to the document's devtools provider on flush
    #[cfg(feature = "devtools")]
    devtools_records: Vec<MutationRecord>,
}

impl Drop for DocumentMutator<'_> {
    fn drop(&mut self) {
        self.flush(); // Defined at bottom of file
        if self.mutations_occurred {
            self.doc.has_changes = true;
            self.doc.shell_provider.request_redraw();
        }
    }
}

impl DocumentMutator<'_> {
    pub fn new<'doc>(doc: &'doc mut BaseDocument) -> DocumentMutator<'doc> {
        DocumentMutator {
            doc,
            eager_op_queue: Vec::new(),
            title_node: None,
            style_nodes: HashSet::new(),
            form_nodes: HashSet::new(),
            recompute_is_animating: false,
            mutations_occurred: false,
            #[cfg(feature = "autofocus")]
            node_to_autofocus: None,
            #[cfg(feature = "devtools")]
            devtools_records: Vec::new(),
        }
    }

    // Query methods

    pub fn node_has_parent(&self, node_id: NodeId) -> bool {
        self.doc.nodes[node_id].parent_id().is_some()
    }

    pub fn previous_sibling_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].backward(1).map(|node| node.id())
    }

    pub fn next_sibling_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].forward(1).map(|node| node.id())
    }

    pub fn parent_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].parent_id()
    }

    pub fn last_child_id(&self, node_id: NodeId) -> Option<NodeId> {
        self.doc.nodes[node_id].children().last()
    }

    pub fn child_ids(&self, node_id: NodeId) -> ThinVec<NodeId> {
        self.doc.nodes[node_id].children().iter().collect()
    }

    /// The element at `node_id`, whose name the parser reads through it.
    pub fn element_name(&self, node_id: NodeId) -> Option<&ElementData> {
        self.doc.nodes[node_id].element_data()
    }

    pub fn node_at_path(&self, start_node_id: NodeId, path: &[u8]) -> NodeId {
        let mut current = &self.doc.nodes[start_node_id];
        for i in path {
            let new_id = current
                .children()
                .get(*i as usize)
                .expect("child path index out of bounds");
            current = &self.doc.nodes[new_id];
        }
        current.id()
    }

    // Node creation methods

    pub fn create_comment_node(&mut self, contents: &str) -> NodeId {
        self.doc.create_node(NodeData::Comment {
            contents: Box::new(contents.to_string()),
        })
    }

    pub fn create_text_node(&mut self, text: &str) -> NodeId {
        self.doc.create_text_node(text)
    }

    pub fn create_element(
        &mut self,
        name: QualName,
        attrs: impl Into<crate::node::Attributes>,
    ) -> NodeId {
        let mut data = ElementData::new(name, attrs);
        // A parsed `<base>` above this element decides its style attribute's url()s.
        if data.attr(local_name!("style")).is_some() {
            self.doc.sync_base_url();
        }
        data.flush_style_attribute(
            self.doc.guard(),
            &self.doc.document_base_url.url_extra_data(),
        );

        // Stylo data is created by the style traversal (`ensure_data`): Stylo treats an
        // element having data as it having been styled.
        self.doc
            .create_node(NodeData::Element(crate::pool::PoolBox::new(data)))
    }

    pub fn deep_clone_node(&mut self, node_id: NodeId) -> NodeId {
        self.doc.deep_clone_node(node_id)
    }

    /// Create (or return the existing) "template contents" fragment node for a
    /// `<template>` element. The contents node is detached from the tree (so it
    /// is inert: never styled or rendered) and is exposed to JavaScript as the
    /// template's `content` DocumentFragment.
    pub fn template_contents(&mut self, template_id: NodeId) -> NodeId {
        if let Some(contents_id) = self.try_template_contents(template_id) {
            return contents_id;
        }
        let name = QualName::new(
            None,
            markup5ever::ns!(html),
            markup5ever::LocalName::from("#document-fragment"),
        );
        let contents_id = self.create_element(name, Vec::new());
        if let Some(element) = self
            .doc
            .get_node_mut(template_id)
            .and_then(|node| node.element_data_mut())
        {
            element.set_template_contents(Some(contents_id));
        }
        contents_id
    }

    /// The "template contents" fragment node of a `<template>` element (if any)
    pub fn try_template_contents(&self, template_id: NodeId) -> Option<NodeId> {
        self.doc
            .get_node(template_id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.template_contents())
    }

    /// Attach (or fetch) a shadow root on `host_id` and mark the host
    /// for restyle so the next paint reflects the new composed tree.
    pub fn attach_shadow(&mut self, host_id: NodeId, mode: ShadowRootMode) -> NodeId {
        let shadow_root_id = self.doc.attach_shadow(host_id, mode);

        let host = &mut self.doc.nodes[host_id];
        host.insert_damage(ALL_DAMAGE);
        if host.flags.is_in_document() {
            if let Some(mut data) = host
                .try_stylo_element_data_mut()
                .and_then(|data| data.get_mut())
            {
                data.hint |= RestyleHint::restyle_subtree();
            }
            host.mark_ancestors_dirty();
        }
        self.maybe_record_node(host_id);

        shadow_root_id
    }

    // Node mutation methods

    /// Set the text of a text or comment node
    pub fn set_node_text(&mut self, node_id: NodeId, value: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        let node = &mut self.doc.nodes[node_id];

        let text = match node.data {
            NodeData::Text(ref mut text) => text,
            // Comments are not rendered, so changing one doesn't damage anything
            NodeData::Comment { ref mut contents } => {
                if **contents != value {
                    **contents = value.to_string();
                    if node_is_in_document {
                        devtools!(
                            self,
                            MutationRecord::CharacterDataChanged(CharacterDataChanged { node_id })
                        );
                    }
                }
                return;
            }
            // TODO: otherwise this is basically element.textContent which is a bit different - need to parse as html
            _ => return,
        };

        let changed = text.content != value;
        if changed {
            self.mutations_occurred |= node_is_in_document;
            text.content.clear();
            text.content.push_str(value);
            node.insert_damage(ALL_DAMAGE);
            if node_is_in_document {
                devtools!(
                    self,
                    MutationRecord::CharacterDataChanged(CharacterDataChanged { node_id })
                );
            }
            // Mark ancestors dirty so the style traversal visits this subtree.
            // Without this, the traversal may skip nodes with pending damage.
            node.mark_ancestors_dirty();
            let parent_id = node.parent_id();

            // Also insert damage on the parent element, since text content changes
            // affect the parent's layout (text may wrap differently, change size, etc.)
            if let Some(parent_id) = parent_id {
                let parent = &mut self.doc.nodes[parent_id];
                parent.insert_damage(ALL_DAMAGE);
            }

            self.doc.note_text_codepoints(value, node_is_in_document);
            self.maybe_record_node(parent_id);
        }
    }

    pub fn append_text_to_node(
        &mut self,
        node_id: NodeId,
        text: &str,
    ) -> Result<(), AppendTextErr> {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        let node = &mut self.doc.nodes[node_id];
        node.insert_damage(ALL_DAMAGE);
        node.mark_ancestors_dirty();
        let parent_id = node.parent_id();
        match node.text_data_mut() {
            Some(data) => {
                data.content += text;
                self.mutations_occurred |= node_is_in_document;
                if node_is_in_document {
                    devtools!(
                        self,
                        MutationRecord::CharacterDataChanged(CharacterDataChanged { node_id })
                    );
                }
                if let Some(parent_id) = parent_id {
                    self.doc.nodes[parent_id].insert_damage(ALL_DAMAGE);
                }
                self.doc.note_text_codepoints(text, node_is_in_document);
                Ok(())
            }
            None => Err(AppendTextErr::NotTextNode),
        }
    }

    pub fn add_attrs_if_missing(&mut self, node_id: NodeId, attrs: Vec<Attribute>) {
        let node = &mut self.doc.nodes[node_id];
        node.insert_damage(ALL_DAMAGE);
        let element_data = node.element_data_mut().expect("Not an element");

        let existing_names = element_data
            .attrs
            .iter()
            .map(|e| e.qual_name())
            .collect::<HashSet<_>>();

        for attr in attrs {
            let name = attr.qual_name();
            if !existing_names.contains(&name) {
                self.set_attribute(node_id, name, &attr.value);
            }
        }
    }

    /// An inline `<svg>` is serialized to usvg once, at box
    /// construction (see construct.rs), so nothing inside it re-renders
    /// unless that root's box is rebuilt. The restyle hint a mutation
    /// leaves behind does not do that: SVG-internal attributes cascade to
    /// nothing, so stylo computes no damage and incremental layout skips
    /// the subtree, leaving a stale snapshot on screen forever.
    fn mark_enclosing_svg_for_reconstruction(&mut self, node_id: NodeId) {
        // Only an SVG-namespaced node can be part of a snapshot, so every
        // HTML write (the overwhelming majority) skips the walk here
        // rather than paying it to reach the root and find nothing.
        // (HTML inside <foreignObject> is exempt: usvg drops it.)
        if !self.doc.nodes[node_id]
            .data
            .downcast_element()
            .is_some_and(|el| *el.namespace() == ns!(svg))
        {
            return;
        }
        // The outermost svg is the one holding the snapshot; nested <svg>
        // elements are serialized as part of it and never construct a box.
        let mut svg_root = None;
        let mut cur = Some(node_id);
        while let Some(id) = cur {
            let node = &self.doc.nodes[id];
            if node
                .data
                .downcast_element()
                .is_some_and(|el| el.name.local == local_name!("svg"))
            {
                svg_root = Some(id);
            }
            cur = node.parent_id();
        }
        if let Some(id) = svg_root {
            self.doc.nodes[id].insert_damage(CONSTRUCT_BOX);
        }
    }

    /// `:dir()` and `:lang()` can depend on this element's inherited state.
    /// Restyle from its parent to include descendants and later `+`/`~` siblings.
    fn restyle_inherited_attr_write(&mut self, node_id: NodeId, name: &QualName) {
        if name.ns != ns!()
            || (name.local != local_name!("dir") && name.local != local_name!("lang"))
            || !self.doc.nodes[node_id].is_element()
        {
            return;
        }

        let scope_id = self.doc.nodes[node_id]
            .parent_id()
            .filter(|&parent_id| self.doc.nodes[parent_id].is_element())
            .unwrap_or(node_id);
        self.doc.nodes[scope_id].set_restyle_hint(RestyleHint::restyle_subtree());
    }

    pub fn set_attribute(&mut self, node_id: NodeId, name: QualName, value: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.note_base_href_write(node_id, &name, node_is_in_document);
        if name.local == local_name!("style") {
            self.doc.sync_base_url();
        }
        if node_is_in_document {
            self.doc.snapshot_node(node_id, &name.local);

            self.restyle_for_attribute_change(node_id, &name.local);
            self.mark_enclosing_svg_for_reconstruction(node_id);
            self.restyle_inherited_attr_write(node_id, &name);
        }

        // Shadow-tree ids stay out of the document id map, as in process_added_subtree.
        if name.local == local_name!("id")
            && node_is_in_document
            && self.doc.nodes[node_id]
                .containing_shadow_root_id()
                .is_none()
        {
            if let Some(old_id) = self.doc.nodes[node_id]
                .element_data()
                .map(|element| element.id.clone())
            {
                if let Some(old_id) = old_id {
                    self.doc.remove_from_id_map(&old_id, node_id);
                }
                self.doc.add_to_id_map(value, node_id);
            }
        }

        let node = &mut self.doc.nodes[node_id];
        let counter_input_changed = name.ns == ns!()
            && node.element_data().is_some_and(|element| {
                (element.name.local == local_name!("ol")
                    && (name.local == local_name!("start")
                        || name.local == local_name!("reversed")))
                    || (element.name.local == local_name!("li")
                        && name.local == local_name!("value"))
            });
        if node_is_in_document && counter_input_changed {
            // Counter resolution reads these attributes outside Stylo's style diff.
            node.insert_damage(CONSTRUCT_BOX);
        }
        let text_field_size_changed = name.ns == ns!()
            && name.local == local_name!("size")
            && node
                .element_data()
                .is_some_and(|element| element.name.local == local_name!("input"));
        if node_is_in_document && text_field_size_changed {
            // Layout reads a text field's size outside Stylo's style diff, so the field would
            // keep the width its cached layout has.
            node.insert_damage(ONLY_RELAYOUT);
        }

        let NodeData::Element(ref mut element) = node.data else {
            return;
        };
        let element: &mut ElementData = element;

        self.mutations_occurred |= node_is_in_document;
        // If element is a CustomWidget, then Ccall attribute_changed on it
        #[cfg(feature = "custom-widget")]
        if matches!(element.special_data(), SpecialElementData::CustomWidget(_)) {
            // Owned: the special data accessor borrows the whole element, attrs included.
            let old_value = element.attrs.get(&name).map(|attr| attr.value.to_string());
            if let Some(SpecialElementData::CustomWidget(widget_data)) = element.special_data_mut()
            {
                widget_data.widget.attribute_changed(
                    &name.local,
                    old_value.as_deref(),
                    Some(value),
                );
            }
        }

        element.attrs.set(name.clone(), value);
        element.presentational_hints.take();
        if node_is_in_document {
            devtools!(
                self,
                MutationRecord::AttributeChanged(AttributeChanged {
                    node_id,
                    name: blitz_traits::SmolStr::new(&*name.local),
                })
            );
        }

        // Focusability is cached on the element and comes from these
        // attributes, so it has to follow a change to one of them: a widget
        // that hands the focus around its own children - a menu, a grid -
        // sets their tabindex after creating them.
        if name.local == local_name!("tabindex")
            || name.local == local_name!("href")
            || name.local == local_name!("disabled")
        {
            element.flush_is_focussable();
        }

        if name.local == local_name!("href") {
            element.flush_link_state();
        }

        if name.local == local_name!("open") {
            element.flush_open_state();
        }

        let tag = &element.name.local;
        let attr = &name.local;

        if *attr == local_name!("id") {
            element.id = Some(Atom::from(value))
        }

        if *attr == local_name!("class") {
            element.classes = crate::node::parse_classes(value);
        }

        if *attr == local_name!("value") {
            if let Some(input_data) = element.text_input_data_mut() {
                // Once JS has written the IDL value the
                // editor is dirty: attribute writes then only
                // change defaultValue and must not repaint (HTML
                // value-attribute-vs-IDL decoupling).
                if input_data.value_override.is_none() {
                    // Update text input value
                    input_data.set_text(
                        &mut self.doc.font_ctx.lock().unwrap(),
                        &mut self.doc.layout_ctx,
                        value,
                    );
                }
            }
            return;
        }

        if name.ns == ns!() && *attr == local_name!("style") {
            let url_data = self.doc.document_base_url.url_extra_data();
            element.flush_style_attribute(&self.doc.guard, &url_data);
            node.set_restyle_hint(RestyleHint::RESTYLE_STYLE_ATTRIBUTE);
            return;
        }

        if *attr == local_name!("disabled") && element.can_be_disabled() {
            node.disable();
            return;
        }

        // Image loads fire for detached <img> elements too:
        // per HTML §update-the-image-data, a relevant mutation starts
        // the image request regardless of connectedness. The
        // `new Image(); img.src = ...` preload pattern and the WPT
        // img.complete async subtests depend on it (the load/error
        // event must fire on the detached element). Deliberately
        // above the is_in_document() early-return below.
        if (tag, attr) == tag_and_attr!("img", "src")
            || (tag, attr) == tag_and_attr!("img", "srcset")
        {
            // srcset edits re-run candidate selection. The src
            // attribute may not change, but the URL we actually
            // fetch can; without re-firing here, pages that hydrate
            // srcset post-mount (BBC, NYT, anything with a
            // responsive-image React component) never load the
            // higher-resolution variant.
            self.load_image(node_id);
            return;
        }

        // If node if not in the document, then don't apply any special behaviours
        // and simply set the attribute value
        if !node.flags.is_in_document() {
            // Except checkedness, which is read back from the checkbox state once it exists
            if (tag, attr) == tag_and_attr!("input", "checked")
                && element.checkbox_input_checked().is_some()
            {
                set_input_checked_state(element, value.to_string());
            }
            return;
        }

        // Custom widgets replace the element's built-in tag-specific behaviour
        #[cfg(feature = "custom-widget")]
        if element.custom_widget_data().is_some() {
            return;
        }

        if (tag, attr) == tag_and_attr!("input", "checked") {
            set_input_checked_state(element, value.to_string());
        } else if (tag, attr) == tag_and_attr!("img", "src")
            || (cfg!(feature = "svg")
                && element.ns() == ns!(svg)
                && matches!(&**tag, "image" | "feImage")
                && *attr == local_name!("href"))
        {
            self.load_image(node_id);
        } else if tag == &local_name!("source") && attr == &local_name!("srcset") {
            // A <source> srcset mutation inside a
            // <picture> needs to re-fire the sibling <img>'s
            // load_image, since the source's srcset takes
            // priority over the img's own src/srcset. Walk to the
            // <picture> parent, then find its <img> descendant.
            if let Some(img_id) = self.find_picture_img_sibling(node_id) {
                self.load_image(img_id);
            }
        } else if (tag, attr) == tag_and_attr!("canvas", "src") {
            self.load_custom_paint_src(node_id);
        } else if (tag, attr) == tag_and_attr!("link", "href")
            || (tag, attr) == tag_and_attr!("link", "rel")
        {
            self.load_linked_stylesheet(node_id);
        } else if (tag, attr) == tag_and_attr!("iframe", "src")
            || (tag, attr) == tag_and_attr!("iframe", "srcdoc")
        {
            self.load_iframe(node_id);
        } else if (tag, attr) == tag_and_attr!("link", "media") {
            self.update_link_stylesheet_media(node_id);
        }

        self.note_possible_slot_reassignment(node_id, &name.local);
    }

    /// `slot` on a host's light child or `name` on a <slot> re-shape
    /// the flat tree; damage the affected host so the next resolve
    /// re-runs assignment, restyle, and box construction there.
    fn note_possible_slot_reassignment(&mut self, node_id: NodeId, attr: &LocalName) {
        if *attr == local_name!("slot") || *attr == local_name!("name") {
            self.doc.slot_assignments_dirty = true;
        }
        if *attr == local_name!("slot") {
            if let Some(parent_id) = self.doc.nodes[node_id].parent_id() {
                if self.doc.nodes[parent_id]
                    .attached_shadow_root_id()
                    .is_some()
                {
                    self.doc.damage_host_subtree(parent_id);
                }
            }
        } else if *attr == local_name!("name") && self.doc.nodes[node_id].is_slot_element() {
            if let Some(root_id) = self.doc.nodes[node_id].containing_shadow_root_id() {
                let host = self.doc.nodes[root_id].shadow_root_data().map(|d| d.host);
                if let Some(host_id) = host {
                    self.doc.damage_host_subtree(host_id);
                }
            }
        }
    }

    /// A connected `<base>`'s href decides the document base URL, so its writes move the epoch.
    fn note_base_href_write(&mut self, node_id: NodeId, name: &QualName, in_document: bool) {
        if in_document
            && name.local == local_name!("href")
            && self.doc.nodes[node_id]
                .data
                .is_element_with_tag_name(&local_name!("base"))
        {
            self.doc.base_epoch = self.doc.base_epoch.wrapping_add(1);
        }
    }

    pub fn clear_attribute(&mut self, node_id: NodeId, name: QualName) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.note_base_href_write(node_id, &name, node_is_in_document);
        if node_is_in_document {
            self.doc.snapshot_node(node_id, &name.local);

            self.restyle_for_attribute_change(node_id, &name.local);
            self.mark_enclosing_svg_for_reconstruction(node_id);
            self.restyle_inherited_attr_write(node_id, &name);
        }

        if name.local == local_name!("id") && node_is_in_document {
            if let Some(old_id) = self.doc.nodes[node_id]
                .element_data()
                .and_then(|element| element.id.clone())
            {
                self.doc.remove_from_id_map(&old_id, node_id);
            }
        }

        let node = &mut self.doc.nodes[node_id];

        let Some(element) = node.element_data_mut() else {
            return;
        };

        let removed_attr = element.attrs.remove(&name);
        let had_attr = removed_attr.is_some();
        if !had_attr {
            return;
        }
        element.presentational_hints.take();
        self.mutations_occurred |= node_is_in_document;
        if node_is_in_document {
            devtools!(
                self,
                MutationRecord::AttributeChanged(AttributeChanged {
                    node_id,
                    name: blitz_traits::SmolStr::new(&*name.local),
                })
            );
        }

        // If element is a CustomWidget, then call attribute_changed on it
        #[cfg(feature = "custom-widget")]
        if let Some(SpecialElementData::CustomWidget(widget_data)) = element.special_data_mut() {
            let old_value = removed_attr.as_ref().map(|attr| &*attr.value);
            widget_data
                .widget
                .attribute_changed(&name.local, old_value, None);
        }

        if name.local == local_name!("id") {
            element.id = None;
        }

        if name.local == local_name!("class") {
            element.classes.clear();
        }

        // As in `set_attribute`: taking one of these away can make the element
        // unfocusable again.
        if name.local == local_name!("tabindex")
            || name.local == local_name!("href")
            || name.local == local_name!("disabled")
        {
            element.flush_is_focussable();
        }

        if name.local == local_name!("href") {
            element.flush_link_state();
        }

        if name.local == local_name!("open") {
            element.flush_open_state();
        }

        // Update text input value
        if name.local == local_name!("value") {
            if let Some(input_data) = element.text_input_data_mut() {
                // Skip when the IDL value is dirty (see
                // the matching guard in set_attribute above).
                if input_data.value_override.is_none() {
                    input_data.set_text(
                        &mut self.doc.font_ctx.lock().unwrap(),
                        &mut self.doc.layout_ctx,
                        "",
                    );
                }
            }
        }

        let tag = &element.name.local;
        let attr = &name.local;

        if *attr == local_name!("disabled") && element.can_be_disabled() {
            node.enable();
            return;
        }

        if name.ns == ns!() && *attr == local_name!("style") {
            let url_data = self.doc.document_base_url.url_extra_data();
            element.flush_style_attribute(&self.doc.guard, &url_data);
            node.set_restyle_hint(RestyleHint::RESTYLE_STYLE_ATTRIBUTE);
        } else if (tag, attr) == tag_and_attr!("input", "checked") {
            element.set_checkbox_input_checked(false);
        } else if (tag, attr) == tag_and_attr!("canvas", "src") {
            self.recompute_is_animating = true;
        } else if (tag, attr) == tag_and_attr!("link", "href") {
            self.unload_stylesheet(node_id);
        } else if (tag, attr) == tag_and_attr!("iframe", "srcdoc") && node_is_in_document {
            // Fall back to loading from the `src` attribute (if any)
            self.load_iframe(node_id);
        } else if (tag, attr) == tag_and_attr!("link", "media") {
            self.update_link_stylesheet_media(node_id);
        }

        self.note_possible_slot_reassignment(node_id, &name.local);
    }

    pub fn set_style_property(&mut self, node_id: NodeId, name: &str, value: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.set_style_property(node_id, name, value);
        self.mutations_occurred |= node_is_in_document;
    }

    pub fn remove_style_property(&mut self, node_id: NodeId, name: &str) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.remove_style_property(node_id, name);
        self.mutations_occurred |= node_is_in_document;
    }

    pub fn set_sub_document(&mut self, node_id: NodeId, sub_document: Box<dyn Document>) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.set_sub_document(node_id, sub_document);
        self.mutations_occurred |= node_is_in_document;
    }

    pub fn remove_sub_document(&mut self, node_id: NodeId) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.remove_sub_document(node_id);
        self.mutations_occurred |= node_is_in_document;
    }

    #[cfg(feature = "custom-widget")]
    pub fn set_custom_widget(&mut self, node_id: NodeId, widget: Box<dyn crate::Widget>) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.set_custom_widget(node_id, widget);
        self.mutations_occurred |= node_is_in_document;
    }

    #[cfg(feature = "custom-widget")]
    pub fn remove_custom_widget(&mut self, node_id: NodeId) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.remove_custom_widget(node_id);
        self.mutations_occurred |= node_is_in_document;
    }

    /// Remove the node from it's parent but don't drop it
    pub fn remove_node(&mut self, node_id: NodeId) {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.slot_assignments_dirty = true;
        // Relative selectors are collected off the still-linked node, so this
        // has to run before either interaction cleanup or unlinking.
        self.doc.invalidate_relative_selectors_for_removal(node_id);

        // Process the subtree *before* severing the parent link so that
        // interaction state referencing removed nodes can retarget to the
        // nearest surviving ancestor.
        self.process_removed_subtree(node_id);

        let node = &mut self.doc.nodes[node_id];

        // Update child_idx values
        if let Some(parent_id) = node.take_parent_id() {
            // A slot-index layout link would outlive the parent's slot; the next resolve
            // re-establishes it under whichever container lays this node out.
            if node.layout_parent_id() == Some(parent_id) {
                node.set_layout_parent_id(None);
            }
            self.mutations_occurred |= node_is_in_document;
            let parent = &mut self.doc.nodes[parent_id];
            parent.insert_damage(ALL_DAMAGE);
            let index = parent.index_of_child(node_id).unwrap_or(0);
            parent.remove_child(node_id);
            if node_is_in_document {
                self.restyle_for_child_list_change(parent_id, index..index);
                devtools!(
                    self,
                    MutationRecord::NodeRemoved(NodeRemoved { parent_id, node_id })
                );
            }
            self.maybe_record_node(parent_id);
        }
    }

    pub fn remove_and_drop_node(&mut self, node_id: NodeId) -> Option<Node> {
        self.remove_and_drop_node_with(node_id, &mut |_| {})
    }

    /// Like [`Self::remove_and_drop_node`], but calls `on_drop` with the id of
    /// every dropped node (the node itself and all of its descendants).
    pub fn remove_and_drop_node_with(
        &mut self,
        node_id: NodeId,
        on_drop: &mut dyn FnMut(NodeId),
    ) -> Option<Node> {
        let node_is_in_document = self.doc.nodes[node_id].flags.is_in_document();
        self.doc.slot_assignments_dirty = true;

        // Must precede the drop; the invalidator walks the node's
        // own ancestors and siblings to find `:has()` anchors.
        self.doc.invalidate_relative_selectors_for_removal(node_id);

        self.process_removed_subtree(node_id);

        let node = self.doc.drop_node_ignoring_parent_with(node_id, on_drop);
        self.mutations_occurred |= node_is_in_document;

        // Update child_idx values
        if let Some(parent_id) = node.as_ref().and_then(|node| node.parent_id()) {
            let parent = &mut self.doc.nodes[parent_id];
            parent.insert_damage(ALL_DAMAGE);
            let parent_is_in_doc = parent.flags.is_in_document();
            let index = parent.index_of_child(node_id).unwrap_or(0);
            parent.remove_child(node_id);

            if parent_is_in_doc {
                self.restyle_for_child_list_change(parent_id, index..index);
                devtools!(
                    self,
                    MutationRecord::NodeRemoved(NodeRemoved { parent_id, node_id })
                );
            }

            self.maybe_record_node(parent_id);
        }

        node
    }

    pub fn remove_and_drop_all_children(&mut self, node_id: NodeId) {
        self.doc.slot_assignments_dirty = true;

        // Same ordering constraint as remove_node, once per child
        // since each is a separate removal as far as `:has()` is concerned.
        for child_id in self.doc.nodes[node_id].children().to_vec() {
            self.doc.invalidate_relative_selectors_for_removal(child_id);
        }

        let parent = &mut self.doc.nodes[node_id];
        let parent_is_in_doc = parent.flags.is_in_document();
        let children = parent.take_children();
        self.mutations_occurred |= parent_is_in_doc && !children.is_empty();

        if parent_is_in_doc {
            self.restyle_for_child_list_change(node_id, 0..0);
        }

        for child_id in children {
            self.process_removed_subtree(child_id);
            let _ = self.doc.drop_node_ignoring_parent(child_id);
            if parent_is_in_doc {
                devtools!(
                    self,
                    MutationRecord::NodeRemoved(NodeRemoved {
                        parent_id: node_id,
                        node_id: child_id,
                    })
                );
            }
        }
        self.maybe_record_node(node_id);
    }

    /// Restyle an in-document element whose attribute is about to change. Only the element
    /// is hinted: its snapshot lets Stylo's invalidator find the siblings and descendants
    /// whose selectors depend on the attribute. `style`, `class` and `id` reach layout only
    /// through the computed style (whose diff yields the damage); other attributes may not.
    fn restyle_for_attribute_change(&mut self, node_id: NodeId, attr: &LocalName) {
        let node = &mut self.doc.nodes[node_id];
        let is_style = *attr == local_name!("style");
        // Attributes that box construction or layout read directly. Any other attribute only
        // reaches layout through the cascade, so its damage comes from the computed-style diff.
        // Inline SVG is rendered from its serialized source, so every SVG attribute counts.
        let is_svg = node
            .element_data()
            .is_some_and(|element| element.ns() == markup5ever::ns!(svg));
        let read_by_layout = is_svg
            || matches!(
                *attr,
                local_name!("type")
                    | local_name!("value")
                    | local_name!("checked")
                    | local_name!("width")
                    | local_name!("height")
                    | local_name!("colspan")
                    | local_name!("rowspan")
                    | local_name!("rows")
                    | local_name!("cols")
                    | local_name!("start")
                    | local_name!("reversed")
                    | local_name!("src")
                    | local_name!("srcdoc")
                    | local_name!("span")
            );
        // A table's `cellpadding` is a presentational hint on its cells, not on the table.
        let restyles_cells = *attr == local_name!("cellpadding")
            && node.data.is_element_with_tag_name(&local_name!("table"));
        if let Some(mut data) = node.try_stylo_element_data_mut().and_then(|s| s.get_mut()) {
            data.hint |= if is_style {
                RestyleHint::RESTYLE_STYLE_ATTRIBUTE
            } else {
                RestyleHint::RESTYLE_SELF
            };
            if restyles_cells {
                data.hint |= RestyleHint::RESTYLE_DESCENDANTS;
            }
            if read_by_layout {
                data.damage.insert(ALL_DAMAGE);
            }
        }
        if read_by_layout {
            node.mark_damaged();
        }
        // Mark ancestors dirty so the style traversal visits this subtree.
        node.mark_ancestors_dirty();
    }

    /// Restyle an in-document element whose child list changed, `changed` being the position
    /// of the inserted children (empty, at the former position of a removed child). New
    /// children are styled by the traversal regardless; the element and its other children
    /// only need restyling where its selector flags record a selector that depends on them.
    fn restyle_for_child_list_change(&mut self, parent_id: NodeId, changed: Range<usize>) {
        let parent = &self.doc.nodes[parent_id];
        let flags = parent.selector_flags().get();
        let mut hint = RestyleHint::empty();
        if flags.intersects(
            ElementSelectorFlags::HAS_EMPTY_SELECTOR
                | ElementSelectorFlags::ANCHORS_RELATIVE_SELECTOR
                | ElementSelectorFlags::ANCHORS_RELATIVE_SELECTOR_NON_SUBJECT,
        ) {
            hint |= RestyleHint::RESTYLE_SELF;
        }
        // A change with no earlier element sibling restyles all children with one hint rather
        // than hinting each later sibling, so inserting or removing at the front stays linear.
        let front_change = flags.contains(ElementSelectorFlags::HAS_SLOW_SELECTOR_LATER_SIBLINGS)
            && !parent
                .children()
                .iter()
                .take(changed.start)
                .any(|id| self.doc.nodes[id].is_element());
        if front_change
            || flags.intersects(
                ElementSelectorFlags::HAS_SLOW_SELECTOR
                    | ElementSelectorFlags::HAS_SLOW_SELECTOR_NTH_OF
                    | ElementSelectorFlags::MAY_HAVE_TREE_COUNTING_FUNCTION,
            )
        {
            hint |= RestyleHint::RESTYLE_DESCENDANTS;
        } else {
            let children = self.doc.nodes[parent_id].children().to_vec();
            let later = if flags.contains(ElementSelectorFlags::HAS_SLOW_SELECTOR_LATER_SIBLINGS) {
                changed.end..children.len()
            } else {
                0..0
            };
            // The element children that were first/last before an insertion, or are after a
            // removal; text nodes don't count for `:first-child`/`:last-child`.
            let mut edge = [None, None];
            if flags.contains(ElementSelectorFlags::HAS_EDGE_CHILD_SELECTOR) {
                let is_element = |id: &NodeId| self.doc.nodes[*id].is_element();
                if !children[..changed.start].iter().any(is_element) {
                    edge[0] = children[changed.end..].iter().copied().find(is_element);
                }
                if !children[changed.end..].iter().any(is_element) {
                    edge[1] = children[..changed.start]
                        .iter()
                        .rev()
                        .copied()
                        .find(is_element);
                }
            }
            for id in children[later].iter().chain(edge.iter().flatten()) {
                self.doc.nodes[*id].set_restyle_hint(RestyleHint::restyle_subtree());
            }
        }

        let parent = &mut self.doc.nodes[parent_id];
        if !hint.is_empty() {
            if let Some(mut data) = parent
                .try_stylo_element_data_mut()
                .and_then(|s| s.get_mut())
            {
                data.hint |= hint;
            }
        }
        // The traversal only descends into an element flagged as having dirty descendants.
        parent.set_dirty_descendants();
        parent.mark_ancestors_dirty();
    }

    // Tree mutation methods
    pub fn remove_node_if_unparented(&mut self, node_id: NodeId) {
        self.remove_node_if_unparented_with(node_id, &mut |_| {});
    }

    /// Like [`Self::remove_node_if_unparented`], but calls `on_drop` with the id of
    /// every dropped node (the node itself and all of its descendants).
    pub fn remove_node_if_unparented_with(
        &mut self,
        node_id: NodeId,
        on_drop: &mut dyn FnMut(NodeId),
    ) {
        if let Some(node) = self.doc.get_node(node_id) {
            if node.parent_id().is_none() {
                self.remove_and_drop_node_with(node_id, on_drop);
            }
        }
    }

    /// Remove all of the children from old_parent_id and append them to new_parent_id
    pub fn append_children(&mut self, parent_id: NodeId, child_ids: &[NodeId]) {
        self.add_children_to_parent(parent_id, child_ids, &|parent, child_ids| {
            let index = parent.children().len();
            parent.extend_children(child_ids.iter().copied());
            index
        });
    }

    pub fn insert_nodes_before(&mut self, anchor_node_id: NodeId, new_node_ids: &[NodeId]) {
        let parent_id = self.doc.nodes[anchor_node_id].parent_id().unwrap();
        self.add_children_to_parent(parent_id, new_node_ids, &|parent, child_ids| {
            let node_child_idx = parent.index_of_child(anchor_node_id).unwrap();
            parent.insert_children(node_child_idx, child_ids.iter().copied());
            node_child_idx
        });
    }

    fn add_children_to_parent(
        &mut self,
        parent_id: NodeId,
        child_ids: &[NodeId],
        insert_children_fn: &dyn Fn(&mut Node, &[NodeId]) -> usize,
    ) {
        // Any child-list mutation can reshape slot assignment (host
        // children, slot fallback, or slot discovery in a shadow tree).
        self.doc.slot_assignments_dirty = true;
        let new_parent_is_in_document = self.doc.nodes[parent_id].flags.is_in_document()
            && !is_template_inert_context(self.doc, parent_id);
        self.mutations_occurred |= new_parent_is_in_document && !child_ids.is_empty();

        // If one of the moved subtrees contains the focused node, the ancestors it is
        // detached from stop matching :focus-within. Record them while the parent links
        // are still intact; the state is transferred after the children are reattached.
        let mut focus_within_old_chain: Option<Vec<NodeId>> = None;
        // A reparent is a removal from the old position, so it needs the same
        // pre-unlink invalidation as remove_node.
        for &child_id in child_ids {
            if self.doc.nodes[child_id].parent_id().is_some() {
                self.doc.invalidate_relative_selectors_for_removal(child_id);
            }
        }

        // Detach the children from their old parents *before* inserting them into
        // the new parent (matching DOM `insertBefore` semantics). If a child is
        // being moved within the same parent then detaching it after insertion
        // would remove both the old and the newly-inserted entries from the
        // parent's child list, and anchor indices would be computed against a
        // child list that still contains the moved nodes.
        //
        // A connected-to-connected move never flips is_in_document, so the id map is not
        // re-derived below. Collect the moved nodes that cross a shadow boundary instead.
        let new_in_shadow = new_parent_is_in_document && {
            let parent = &self.doc.nodes[parent_id];
            parent.is_shadow_root() || parent.containing_shadow_root_id().is_some()
        };
        let mut crossed_shadow_boundary = Vec::new();
        for child_id in child_ids.iter().copied() {
            let child = &self.doc.nodes[child_id];
            if child.contains_focus() {
                focus_within_old_chain =
                    Some(self.doc.maybe_node_element_ancestors(child.parent_id()));
            }

            let child = &mut self.doc.nodes[child_id];
            let child_was_in_doc = child.flags.is_in_document();
            self.mutations_occurred |= child_was_in_doc;
            // A connected move can change which `base[href]` comes first even when the moved
            // node is not the base itself, so every connected move invalidates the cached URL.
            if child_was_in_doc {
                self.doc.base_epoch = self.doc.base_epoch.wrapping_add(1);
            }
            if new_parent_is_in_document
                && child_was_in_doc
                && child.containing_shadow_root_id().is_some() != new_in_shadow
            {
                crossed_shadow_boundary.push(child_id);
            }
            // Reparenting invalidates any slot assignment; the next
            // resolve's flush_slot_assignments re-establishes it.
            child.set_assigned_slot(None);
            let Some(old_parent_id) = child.take_parent_id() else {
                continue;
            };

            if self.doc.nodes[child_id].layout_parent_id() == Some(old_parent_id) {
                // A slot-index link would outlive the old parent's slot; the next resolve
                // re-establishes it under whichever container ends up laying this node out.
                self.doc.nodes[child_id].set_layout_parent_id(None);
            }

            let old_parent = &mut self.doc.nodes[old_parent_id];
            old_parent.insert_damage(ALL_DAMAGE);
            let index = old_parent.index_of_child(child_id).unwrap_or(0);
            old_parent.remove_child(child_id);

            if child_was_in_doc {
                self.restyle_for_child_list_change(old_parent_id, index..index);
                devtools!(
                    self,
                    MutationRecord::NodeRemoved(NodeRemoved {
                        parent_id: old_parent_id,
                        node_id: child_id,
                    })
                );
            }

            self.maybe_record_node(old_parent_id);
        }

        let has_relative_selectors = new_parent_is_in_document && self.doc.has_relative_selectors();
        let new_parent = &mut self.doc.nodes[parent_id];
        new_parent.insert_damage(ALL_DAMAGE);
        let index = insert_children_fn(new_parent, child_ids);

        let restyle_whole_parent = new_parent_is_in_document
            && (insertion_restyles_whole_parent(new_parent) || has_relative_selectors);
        if new_parent_is_in_document {
            self.restyle_for_child_list_change(parent_id, index..index + child_ids.len());
        }
        // A `:has()` starting with `+`/`~`, or a `<bdi>`/`dir=auto` parent whose direction comes
        // from its descendants' text, can change for the whole parent.
        if restyle_whole_parent {
            self.doc.nodes[parent_id].set_restyle_hint(RestyleHint::restyle_subtree());
        }

        for child_id in child_ids.iter().copied() {
            let child = &mut self.doc.nodes[child_id];
            let child_was_in_doc = child.flags.is_in_document();
            child.set_parent_id(Some(parent_id));
            // A moved or re-attached subtree keeps its styles, but everything above it changed.
            if new_parent_is_in_document && child.primary_styles().is_some() {
                child.set_restyle_hint(RestyleHint::restyle_subtree());
            }

            if new_parent_is_in_document {
                devtools!(
                    self,
                    MutationRecord::NodeInserted(NodeInserted {
                        parent_id,
                        node_id: child_id,
                        previous_sibling_id: self.devtools_previous_sibling(parent_id, child_id),
                    })
                );
            }

            if new_parent_is_in_document && !child_was_in_doc {
                self.process_added_subtree(child_id);
            } else if !new_parent_is_in_document && child_was_in_doc {
                self.process_removed_subtree(child_id);
            }
        }
        for child_id in crossed_shadow_boundary {
            self.reindex_subtree_ids(child_id, new_in_shadow);
        }

        // Transfer :focus-within from the old ancestor chain to the new one (the
        // shared prefix stays). If the subtree left the document, the focus was
        // cleared with it and only the old chain needs the state removed.
        if let Some(old_chain) = focus_within_old_chain {
            let new_chain = match self.doc.focus_node_id {
                Some(_) => self.doc.node_element_ancestors(parent_id),
                None => Vec::new(),
            };
            let same_count = old_chain
                .iter()
                .zip(&new_chain)
                .take_while(|(o, n)| o == n)
                .count();
            for &id in old_chain.iter().skip(same_count) {
                self.doc
                    .snapshot_node_and(id, ElementState::FOCUS_WITHIN, |node| {
                        node.remove_focus_within()
                    });
            }
            for &id in new_chain.iter().skip(same_count) {
                self.doc
                    .snapshot_node_and(id, ElementState::FOCUS_WITHIN, |node| {
                        node.add_focus_within()
                    });
            }
        }

        // Insertions invalidate after linking, since the collector
        // reads the node's final parent and sibling positions.
        if new_parent_is_in_document {
            for &child_id in child_ids.iter() {
                self.doc
                    .invalidate_relative_selectors_for_insertion(child_id);
            }
        }

        self.maybe_record_node(parent_id);
    }

    /// The nearest preceding sibling of `node_id` in `parent_id`'s children
    /// that devtools show (not a whitespace-only text node or anonymous block)
    #[cfg(feature = "devtools")]
    fn devtools_previous_sibling(&self, parent_id: NodeId, node_id: NodeId) -> Option<NodeId> {
        let parent = &self.doc.nodes[parent_id];
        let index = parent.index_of_child(node_id)?;
        parent.children[..index].iter().rev().copied().find(|id| {
            let node = &self.doc.nodes[*id];
            !node.is_whitespace_node() && !node.is_anonymous()
        })
    }

    // Tree mutation methods (that defer to other methods)
    pub fn insert_nodes_after(&mut self, anchor_node_id: NodeId, new_node_ids: &[NodeId]) {
        match self.next_sibling_id(anchor_node_id) {
            Some(id) => self.insert_nodes_before(id, new_node_ids),
            None => {
                let parent_id = self.parent_id(anchor_node_id).unwrap();
                self.append_children(parent_id, new_node_ids)
            }
        }
    }

    pub fn reparent_children(&mut self, old_parent_id: NodeId, new_parent_id: NodeId) {
        let child_ids = self.doc.nodes[old_parent_id].take_children();
        self.maybe_record_node(old_parent_id);
        self.append_children(new_parent_id, &child_ids);
    }

    pub fn replace_node_with(&mut self, anchor_node_id: NodeId, new_node_ids: &[NodeId]) {
        // Self-replace short-circuit.
        // `replaceChild(x, x)` (same node on both sides) should
        // leave the tree unchanged (per DOM §4.2.3 "replace"
        // semantics; the JS shim emits the MutationObserver
        // records before reaching wasm, so we don't need to
        // simulate the work here).
        //
        // Without this guard, the post-detach refactor below would
        // call `index_of_child(anchor)` after `anchor` had been
        // detached, returning None and panicking on unwrap. The
        // original (pre-detach) code also corrupted the tree on
        // self-replace: splice inserted a duplicate, then
        // `retain` removed every occurrence including the
        // anchor, then `remove_node(anchor)` no-op'd because the
        // anchor was already gone.
        if new_node_ids.len() == 1 && new_node_ids[0] == anchor_node_id {
            return;
        }
        self.insert_nodes_before(anchor_node_id, new_node_ids);
        self.remove_node(anchor_node_id);
    }

    // === ParentNode / ChildNode mixin mutation methods ===
    //
    // These implement the DOM spec's mutation semantics for the ParentNode and
    // ChildNode mixins (`append`/`prepend`/`replaceChildren` and
    // `before`/`after`/`replaceWith`). Callers pass lists of node ids with
    // string arguments already converted to (detached) text nodes and
    // DocumentFragment arguments already expanded into their children (per the
    // DOM spec's "insert a node" steps).

    /// Detach (rather than drop) any already-parented nodes, so that references
    /// to them (and their descendants) remain valid
    fn detach_all(&mut self, node_ids: &[NodeId]) {
        for node_id in node_ids {
            if self.node_has_parent(*node_id) {
                self.remove_node(*node_id);
            }
        }
    }

    /// The nearest sibling of `anchor_id` (in the direction given by `offset`:
    /// +1 = following, -1 = preceding) which is not in `excluded` — the spec's
    /// "viable next/previous sibling" for ChildNode mutation methods
    fn viable_sibling(
        &self,
        anchor_id: NodeId,
        offset: isize,
        excluded: &[NodeId],
    ) -> Option<NodeId> {
        let node = self.doc.get_node(anchor_id)?;
        let parent = self.doc.get_node(node.parent_id()?)?;
        let mut index = parent.index_of_child(anchor_id)?;
        loop {
            index = index.checked_add_signed(offset)?;
            let sibling_id = parent.children().get(index)?;
            if !excluded.contains(&sibling_id) {
                return Some(sibling_id);
            }
        }
    }

    /// ParentNode's `prepend()`: insert the nodes at the start of `parent_id`'s
    /// children (ParentNode's `append()` is [`append_children`](Self::append_children))
    pub fn prepend_nodes(&mut self, parent_id: NodeId, node_ids: &[NodeId]) {
        self.detach_all(node_ids);
        match self.child_ids(parent_id).first().copied() {
            Some(first_child_id) => self.insert_nodes_before(first_child_id, node_ids),
            None => self.append_children(parent_id, node_ids),
        }
    }

    /// ParentNode's `replaceChildren()`: replace all of `parent_id`'s children
    /// with the given nodes. The existing children are detached rather than
    /// dropped, so that references to them remain valid.
    pub fn replace_children(&mut self, parent_id: NodeId, node_ids: &[NodeId]) {
        self.detach_all(node_ids);
        for child_id in self.child_ids(parent_id) {
            self.remove_node(child_id);
        }
        self.append_children(parent_id, node_ids);
    }

    /// ChildNode's `before()`: insert the nodes before `anchor_id`. The
    /// insertion point is after the nearest preceding sibling which isn't
    /// itself being inserted (or at the start of the parent if there is none).
    /// Does nothing if the anchor has no parent.
    pub fn before_node(&mut self, anchor_id: NodeId, node_ids: &[NodeId]) {
        let Some(parent_id) = self.parent_id(anchor_id) else {
            return;
        };
        let viable_prev = self.viable_sibling(anchor_id, -1, node_ids);
        self.detach_all(node_ids);
        match viable_prev {
            Some(prev_id) => self.insert_nodes_after(prev_id, node_ids),
            None => match self.child_ids(parent_id).first().copied() {
                Some(first_child_id) => self.insert_nodes_before(first_child_id, node_ids),
                None => self.append_children(parent_id, node_ids),
            },
        }
    }

    /// ChildNode's `after()`: insert the nodes after `anchor_id`. The insertion
    /// point is before the nearest following sibling which isn't itself being
    /// inserted (or at the end of the parent if there is none). Does nothing if
    /// the anchor has no parent.
    pub fn after_node(&mut self, anchor_id: NodeId, node_ids: &[NodeId]) {
        let Some(parent_id) = self.parent_id(anchor_id) else {
            return;
        };
        let viable_next = self.viable_sibling(anchor_id, 1, node_ids);
        self.detach_all(node_ids);
        match viable_next {
            Some(next_id) => self.insert_nodes_before(next_id, node_ids),
            None => self.append_children(parent_id, node_ids),
        }
    }

    /// ChildNode's `replaceWith()`: replace `anchor_id` with the nodes in its
    /// parent's child list. If the anchor is itself one of the inserted nodes,
    /// they are inserted at its old position (the spec's "viable next sibling"
    /// handling). The anchor is detached rather than dropped, so references to
    /// it remain valid. Does nothing if the anchor has no parent.
    pub fn replace_with_nodes(&mut self, anchor_id: NodeId, node_ids: &[NodeId]) {
        let Some(parent_id) = self.parent_id(anchor_id) else {
            return;
        };
        let viable_next = self.viable_sibling(anchor_id, 1, node_ids);
        self.detach_all(node_ids);
        if self.node_has_parent(anchor_id) {
            self.replace_node_with(anchor_id, node_ids);
        } else {
            match viable_next {
                Some(next_id) => self.insert_nodes_before(next_id, node_ids),
                None => self.append_children(parent_id, node_ids),
            }
        }
    }
}

impl<'doc> DocumentMutator<'doc> {
    pub fn flush(&mut self) {
        if self.recompute_is_animating {
            self.doc.has_canvas = self.doc.compute_has_canvas();
        }

        if let Some(id) = self.title_node {
            let title = self.doc.nodes[id].text_content();
            self.doc.shell_provider.set_window_title(title);
        }

        // Add/Update inline stylesheets (<style> elements)
        // A first sheet is parsed now so its @import and @font-face loads start
        // early; replacing an already registered one waits for resolve.
        for id in self.style_nodes.drain() {
            if self.doc.nodes_to_stylesheet.contains_key(&id) {
                self.doc.pending_style_nodes.insert(id);
            } else {
                self.doc.process_style_element(id);
            }
        }

        // Snapshot form_nodes so we can both reset_form_owner
        // and sync_select_label without double-draining the set.
        let form_ids: Vec<NodeId> = self.form_nodes.drain().collect();
        for &id in &form_ids {
            self.doc.reset_form_owner(id);
        }
        // Closed `<select>` paints the selected
        // option's text. Blitz has no native form-control
        // rendering, so without this every <select> renders as
        // an empty inline-block (just the bootstrap chrome /
        // chevron from author CSS).
        //
        // We run at flush() rather than from process_button_input
        // because flush() runs once per mutate session (Drop of
        // DocumentMutator): after the HTML parser has finished
        // attaching all children of all <select>s. Running per-
        // element from process_button_input would either fire
        // before options exist (parser appends them one at a
        // time after the select open tag is processed) or fire
        // multiple times against intermediate states with extra
        // bookkeeping for idempotency. flush()-time gives one
        // clean pass against the final parsed tree.
        //
        // Selection rule: first `<option>` with the `selected`
        // content attribute wins; otherwise the first `<option>`
        // in document order. Matches HTML §form-controls
        // initial selectedness for a single-select. Multi-
        // selects (`<select multiple>`) and explicit `size`
        // listboxes aren't special-cased: they render their
        // first selected option's text as a one-line label,
        // which is wrong for the listbox shape but no worse
        // than the all-empty baseline.
        //
        // Caveat: this runs at parse time only. Later JS
        // mutations (`select.value = ...`, `option.selected =
        // true`, `select.add(...)`) won't refresh the label.
        // Acceptable for the static-screenshot path; an
        // interactive sync hook would belong in the dom-shim's
        // HTMLSelectElement setters.
        for id in form_ids {
            self.sync_select_label_from_attrs(id);
        }

        #[cfg(feature = "autofocus")]
        if let Some(node_id) = self.node_to_autofocus.take() {
            if self.doc.get_node(node_id).is_some() {
                self.doc.set_focus_to(node_id);
            }
        }

        #[cfg(feature = "devtools")]
        if !self.devtools_records.is_empty() {
            if let Some(provider) = &self.doc.devtools_provider {
                provider.mutations(&self.devtools_records);
            }
            self.devtools_records.clear();
        }
    }

    pub fn set_inner_html(&mut self, node_id: NodeId, html: &str) {
        // The embedder may still hold wrappers for replaced children. Detach
        // them here; the DOM shim drops only subtrees it can prove are
        // unwrapped after this operation returns.
        let old_children = self.doc.nodes[node_id].children().to_vec();
        for child_id in old_children {
            self.remove_node(child_id);
        }

        // html5ever's fragment parser needs an element context (its
        // elem_name callback panics otherwise). Shadow roots parse
        // under a temp element named like the host, then adopt.
        let shadow_host = self.doc.nodes[node_id]
            .shadow_root_data()
            .map(|data| data.host);
        if let Some(host_id) = shadow_host {
            let host_name = self.doc.nodes[host_id]
                .element_data()
                .expect("shadow host must be an element")
                .qual_name();
            let tmp_id = self.create_element(host_name, Vec::new());
            self.doc
                .html_parser_provider
                .clone()
                .parse_inner_html(self, tmp_id, html);
            let child_ids = self.child_ids(tmp_id);
            self.append_children(node_id, &child_ids);
            self.remove_and_drop_node(tmp_id);
            return;
        }

        self.doc
            .html_parser_provider
            .clone()
            .parse_inner_html(self, node_id, html);
    }

    fn flush_eager_ops(&mut self) {
        let mut ops = mem::take(&mut self.eager_op_queue);
        for op in ops.drain(0..) {
            match op {
                SpecialOp::LoadImage(node_id) => self.load_image(node_id),
                SpecialOp::LoadIframe(node_id) => self.load_iframe(node_id),
                SpecialOp::LoadStylesheet(node_id) => self.load_linked_stylesheet(node_id),
                SpecialOp::UnloadStylesheet(node_id) => self.unload_stylesheet(node_id),
                SpecialOp::LoadCustomPaintSource(node_id) => self.load_custom_paint_src(node_id),
                SpecialOp::ProcessButtonInput(node_id) => self.process_button_input(node_id),
                SpecialOp::UnloadSubDocument(node_id) => self.remove_sub_document(node_id),
                #[cfg(feature = "custom-widget")]
                SpecialOp::UnloadCustomWidget(node_id) => self.remove_custom_widget(node_id),
            }
        }

        // Queue is empty, but put Vec back anyway so allocation can be reused.
        self.eager_op_queue = ops;
    }

    /// Re-derives id map membership for a connected subtree whose shadow containment changed:
    /// only connected light-tree ids belong in the map. `in_shadow` is the root's containment;
    /// each descendant derives its own from its traversal parent, so no id node walks its
    /// ancestors.
    fn reindex_subtree_ids(&mut self, node_id: NodeId, in_shadow: bool) {
        let node = &self.doc.nodes[node_id];
        if let Some(id_attr) = node.attr(local_name!("id")).map(ToString::to_string) {
            let indexed = node.flags.is_in_document() && !in_shadow;
            self.doc.remove_from_id_map(&id_attr, node_id);
            if indexed {
                self.doc.add_to_id_map(&id_attr, node_id);
            }
        }

        // A shadow root's parent is its host, so it shares the host's containment.
        let children_in_shadow = in_shadow || self.doc.nodes[node_id].is_shadow_root();
        if let Some(shadow_root_id) = self.doc.nodes[node_id].attached_shadow_root_id() {
            self.reindex_subtree_ids(shadow_root_id, in_shadow);
        }
        for index in 0..self.doc.nodes[node_id].children().len() {
            let child_id = self.doc.nodes[node_id]
                .children()
                .get(index)
                .expect("index is below the child count");
            self.reindex_subtree_ids(child_id, children_in_shadow);
        }
    }

    fn process_added_subtree(&mut self, node_id: NodeId) {
        self.doc.iter_subtree_mut(node_id, |node_id, doc| {
            let in_template_contents = doc.nodes[node_id]
                .parent_id()
                .is_some_and(|parent_id| is_template_inert_context(doc, parent_id));
            let in_document = !in_template_contents;
            if in_document {
                if let Some(text) = doc.nodes[node_id].text_data() {
                    crate::document::note_connected_text_codepoints(
                        &mut doc.used_codepoints,
                        &mut doc.non_latin_text_writes,
                        &text.content,
                    );
                }
            }
            let node = &mut doc.nodes[node_id];
            node.flags.set(NodeFlags::IS_IN_DOCUMENT, in_document);
            node.insert_damage(ALL_DAMAGE);
            if in_template_contents {
                return;
            }

            // If the node has an "id" attribute, store it in the ID map.
            if let Some(id_attr) = node.attr(local_name!("id")).map(Atom::from) {
                if node.containing_shadow_root_id().is_none() {
                    doc.add_atom_to_id_map(id_attr, node_id);
                }
            }

            let node = &mut doc.nodes[node_id];
            let NodeData::Element(ref mut element) = node.data else {
                return;
            };

            // Custom widgets replace the element's built-in tag-specific behaviour
            #[cfg(feature = "custom-widget")]
            let has_custom_widget = element.custom_widget_data().is_some();
            #[cfg(not(feature = "custom-widget"))]
            let has_custom_widget = false;

            // Custom post-processing by element tag name
            let tag = element.name.local.as_ref();
            match tag {
                "title" => self.title_node = Some(node_id),
                "style" => {
                    self.style_nodes.insert(node_id);
                }
                "base" => {
                    doc.base_epoch = doc.base_epoch.wrapping_add(1);
                    doc.connected_bases.insert(node_id);
                }
                _ if has_custom_widget => {}
                "link" => self.eager_op_queue.push(SpecialOp::LoadStylesheet(node_id)),
                "img" => self.eager_op_queue.push(SpecialOp::LoadImage(node_id)),
                "image" | "feImage" if cfg!(feature = "svg") && element.ns() == ns!(svg) => {
                    self.eager_op_queue.push(SpecialOp::LoadImage(node_id))
                }
                "iframe" => self.eager_op_queue.push(SpecialOp::LoadIframe(node_id)),
                "canvas" => self
                    .eager_op_queue
                    .push(SpecialOp::LoadCustomPaintSource(node_id)),
                "button" | "fieldset" | "input" | "select" | "textarea" | "object" | "output" => {
                    self.eager_op_queue
                        .push(SpecialOp::ProcessButtonInput(node_id));
                    self.form_nodes.insert(node_id);
                }
                _ => {}
            }

            #[cfg(feature = "autofocus")]
            if node.is_focussable() {
                if let NodeData::Element(ref element) = node.data {
                    // `autofocus` is a boolean attribute, so presence alone enables it
                    // (`<input autofocus>`). "false" is still ignored, because Dioxus
                    // writes `autofocus: false` as the string "false".
                    if let Some(value) = element.attr(local_name!("autofocus")) {
                        if value != "false" {
                            self.node_to_autofocus = Some(node_id);
                        }
                    }
                }
            }
        });

        self.flush_eager_ops();
    }

    fn process_removed_subtree(&mut self, node_id: NodeId) {
        self.doc.iter_subtree_mut(node_id, |node_id, doc| {
            doc.reset_node_scroll_state(node_id);
            doc.nodes[node_id]
                .flags
                .set(NodeFlags::IS_IN_DOCUMENT, false);

            // Clear any interaction state that references this node, running
            // the usual teardown steps (unhover/unactive the surviving
            // ancestor chain, IME disable on blur of a focused input).
            doc.clear_interaction_state_for_removed_node(node_id);

            if doc.nodes[node_id]
                .data
                .is_element_with_tag_name(&local_name!("base"))
            {
                doc.base_epoch = doc.base_epoch.wrapping_add(1);
                doc.connected_bases.remove(&node_id);
            }
            if !doc.linked_stylesheet_urls.is_empty() {
                doc.linked_stylesheet_urls.remove(&node_id);
            }
            let is_style = doc.nodes[node_id]
                .data
                .is_element_with_tag_name(&local_name!("style"));
            if is_style {
                doc.pending_style_nodes.remove(&node_id);
                doc.remove_author_stylesheet(node_id);
            }

            // A removed slot's assignees live outside this subtree, and the flat-tree
            // dirty walks follow assigned_slot: a freed slot would be a dead key.
            if let Some(assignees) = doc.nodes[node_id].take_assigned_nodes() {
                for assignee in assignees {
                    if let Some(slotted) = doc.nodes.get_mut(assignee) {
                        if slotted.assigned_slot() == Some(node_id) {
                            slotted.set_assigned_slot(None);
                        }
                    }
                }
            }

            let node = &mut doc.nodes[node_id];
            // Stale flat-tree edges must not outlive removal: slab ids
            // recycle, so a dangling slot pointer would mislead.
            node.set_assigned_slot(None);

            // Clear the text selection if one of its endpoints references this node.
            // This prevents stale selection endpoint references.
            if doc.text_selection.anchor.node_or_parent == Some(node_id)
                || doc.text_selection.focus.node_or_parent == Some(node_id)
            {
                doc.text_selection.clear();
            }

            // Remove any snapshot for this node to prevent stale snapshot references
            // during style invalidation.
            if node.has_snapshot() {
                let opaque_id = node.opaque();
                doc.snapshots.remove(&opaque_id);
                node.set_has_snapshot(false);
            }

            // If the node has an "id" attribute remove it from the ID map.
            if let Some(id_attr) = node.attr(local_name!("id")).map(ToString::to_string) {
                doc.remove_from_id_map(&id_attr, node_id);
            }

            let node = &mut doc.nodes[node_id];
            let NodeData::Element(ref mut element) = node.data else {
                return;
            };

            match element.special_data() {
                SpecialElementData::SubDocument(_) => {
                    self.eager_op_queue
                        .push(SpecialOp::UnloadSubDocument(node_id));
                }
                #[cfg(feature = "custom-widget")]
                SpecialElementData::CustomWidget(_) => {
                    self.eager_op_queue
                        .push(SpecialOp::UnloadCustomWidget(node_id));
                }
                SpecialElementData::Stylesheet(_) => self
                    .eager_op_queue
                    .push(SpecialOp::UnloadStylesheet(node_id)),
                SpecialElementData::Image(_) => {}
                SpecialElementData::Canvas(_) => {
                    self.recompute_is_animating = true;
                }
                SpecialElementData::TableRoot(_) => {}
                SpecialElementData::TextInput(_) => {}
                SpecialElementData::CheckboxInput(_) => {}
                SpecialElementData::RangeInput(_) => {}
                #[cfg(feature = "file-input")]
                SpecialElementData::FileInput(_) => {}
                SpecialElementData::None => {}
            }
        });

        self.flush_eager_ops();
    }

    fn maybe_record_node(&mut self, node_id: impl Into<Option<NodeId>>) {
        let Some(node_id) = node_id.into() else {
            return;
        };

        let Some(tag_name) = self.doc.nodes[node_id]
            .data
            .downcast_element()
            .map(|elem| &elem.name.local)
        else {
            return;
        };

        match tag_name.as_ref() {
            "title" => self.title_node = Some(node_id),
            "style" => {
                self.style_nodes.insert(node_id);
            }
            _ => {}
        }
    }

    fn load_linked_stylesheet(&mut self, target_id: NodeId) {
        self.doc.sync_base_url();
        self.doc.linked_stylesheet_urls.remove(&target_id);
        let node = &self.doc.nodes[target_id];

        let mut is_in_head = false;
        let mut parent_id = node.parent_id();
        while let Some(id) = parent_id
            && !is_in_head
        {
            let parent = &self.doc.nodes[id];
            is_in_head |= parent.data.is_element_with_tag_name(&local_name!("head"));
            parent_id = parent.parent_id();
        }

        let rel_attr = node.attr(local_name!("rel"));
        let href_attr = node.attr(local_name!("href"));

        let (Some(rels), Some(href)) = (rel_attr, href_attr) else {
            return;
        };
        if !rels
            .split_ascii_whitespace()
            .any(|rel| rel.eq_ignore_ascii_case("stylesheet"))
        {
            return;
        }

        // A `crossorigin` attribute (any value, including the
        // empty string) puts the CSS fetch in mode "cors"; its absence
        // means "no-cors", whose response is opaque even when the server
        // does send Access-Control-Allow-Origin. HTML §link-type-stylesheet.
        let cors_mode = match node.attr(LocalName::from("crossorigin")) {
            Some(_) => "cors",
            None => "no-cors",
        };

        // Carry the media attribute to the parse so the built
        // sheet is scoped to it (print sheets must not apply on screen).
        let media_attr = node.attr(local_name!("media")).map(ToString::to_string);

        let Some(url) = self.doc.resolve_url(href) else {
            return;
        };
        self.doc
            .linked_stylesheet_urls
            .insert(target_id, url.to_string());
        let handler = ResourceHandler::new(
            self.doc.tx.clone(),
            self.doc.id(),
            Some(node.id()),
            self.doc.shell_provider.clone(),
            StylesheetHandler {
                source_url: url.clone(),
                guard: self.doc.guard.clone(),
                net_provider: self.doc.net_provider.clone(),
                abort_signal: self.doc.abort_signal.clone(),
                cors_mode: Some(String::from(cors_mode)),
                media_attr,
            },
        );

        if is_in_head && !self.doc.net_provider.is_noop() {
            self.doc
                .pending_critical_resources
                .insert(handler.request_id());
        }

        // Tag the Request with `content_type = "text/css"`
        // so embedders (KitesurfNetProvider) can distinguish stylesheet
        // loads from generic resource loads without having to peek at
        // the NetHandler trait object. Blitz's NetProvider trait
        // doesn't expose the handler type, and Request has no
        // `kind` field, so content_type is the cleanest signal.
        //
        // The `ks-cors` MIME parameter rides along so the embedder can
        // tell a CORS-mode sheet from an opaque one.
        let mut req = self.doc.build_request(url);
        req.content_type = Some(crate::net::css_content_type(Some(cors_mode)));
        self.doc
            .net_provider
            .fetch(self.doc.id(), req, Box::new(handler));
    }

    // A <link media> mutation re-scopes the loaded sheet
    // in place; the async-CSS pattern (media="print" + onload flipping
    // to "all") depends on this. Media was only snapshotted at fetch
    // time before, so the flip never re-cascaded the new scope.
    fn update_link_stylesheet_media(&mut self, node_id: NodeId) {
        let node = &self.doc.nodes[node_id];
        let media_attr = node.attr(local_name!("media")).map(ToString::to_string);
        let Some((sheet, scope)) = self.doc.nodes_to_stylesheet.get(&node_id).cloned() else {
            // No installed sheet (fetch in flight or never fired):
            // re-fire so the handler snapshots the NEW media value.
            if self.doc.nodes[node_id].flags.is_in_document() {
                self.load_linked_stylesheet(node_id);
            }
            return;
        };
        let media_list = crate::net::parse_media_attr(
            media_attr.as_deref(),
            &self.doc.url.url_extra_data(),
            style::stylesheets::Origin::Author,
        );
        {
            let mut guard = self.doc.guard.write();
            *sheet.0.media.write_with(&mut guard) = media_list;
        }
        // Stylo caches media evaluation in cascade data, so a pure
        // MediaList swap needs a forced origin rebuild to recascade.
        match scope {
            Some(shadow_root_id) => self.doc.note_shadow_style_change(shadow_root_id),
            None => self
                .doc
                .stylist
                .force_stylesheet_origins_dirty(OriginSet::all()),
        }
    }

    fn unload_stylesheet(&mut self, node_id: NodeId) {
        let node = &mut self.doc.nodes[node_id];
        let Some(element) = node.element_data_mut() else {
            unreachable!();
        };
        // An icon link never holds a stylesheet, and a stylesheet link holds none
        // until its fetch has completed.
        let stylesheet = match element.take_special_data() {
            SpecialElementData::Stylesheet(stylesheet) => stylesheet,
            other => {
                element.set_special_data(other);
                return;
            }
        };

        // Unregister from wherever the sheet was recorded as living
        // (the node may already be detached when the unload op runs).
        let recorded_scope = self
            .doc
            .nodes_to_stylesheet
            .remove(&node_id)
            .and_then(|(_, scope)| scope);
        match recorded_scope {
            Some(shadow_root_id) => {
                self.doc
                    .remove_shadow_stylesheet(shadow_root_id, stylesheet);
            }
            None => {
                let guard = self.doc.guard.read();
                self.doc.stylist.remove_stylesheet(stylesheet, &guard);
                drop(guard);
                self.doc
                    .stylist
                    .force_stylesheet_origins_dirty(OriginSet::all());
            }
        }
        self.doc.stylesheet_generation += 1;
    }

    /// Walk from a `<source>` node up to its enclosing
    /// `<picture>` parent, then return the first `<img>` child of
    /// that picture. Used by the attribute-mutation handler to
    /// re-fire `load_image` when a `<source>`'s srcset changes
    /// post-mount.
    fn find_picture_img_sibling(&self, source_id: NodeId) -> Option<NodeId> {
        let parent_id = self.doc.nodes[source_id].parent_id()?;
        let parent = &self.doc.nodes[parent_id];
        if !parent
            .data
            .is_element_with_tag_name(&local_name!("picture"))
        {
            return None;
        }
        for child_id in parent.children() {
            if self.doc.nodes[child_id]
                .data
                .is_element_with_tag_name(&local_name!("img"))
            {
                return Some(child_id);
            }
        }
        None
    }

    fn is_supported_picture_source_type(type_hint: &str) -> bool {
        // Match the image crate's enabled codecs and this crate's optional SVG parser.
        // Like browsers, treat an empty type as supported and accept legacy aliases.
        let essence = type_hint.split(';').next().unwrap_or_default().trim();
        essence.is_empty()
            || [
                "image/png",
                "image/x-png",
                "image/apng",
                "image/jpeg",
                "image/jpg",
                "image/pjpeg",
                "image/gif",
                "image/webp",
            ]
            .iter()
            .any(|supported| essence.eq_ignore_ascii_case(supported))
            || (cfg!(feature = "svg") && essence.eq_ignore_ascii_case("image/svg+xml"))
    }

    /// Pick the URL to actually fetch for an `<img>`.
    ///
    /// Precedence (matches WHATWG HTML §4.8.4.3 "Updating the
    /// image data", trimmed to what real pages depend on):
    ///
    /// 1. If the `<img>` is inside a `<picture>`, walk `<source>` siblings in
    ///    document order. Skip sources whose `type` names an unsupported MIME.
    /// 2. Otherwise, parse the `<img>`'s own `srcset` and pick a
    ///    candidate.
    /// 3. Otherwise, fall back to the `<img>`'s `src` attribute.
    ///
    /// This does not yet honour `<source media="...">` or `sizes`; it picks
    /// the largest descriptor for headless rendering.
    fn select_image_url(&self, target_id: NodeId) -> Option<String> {
        let node = &self.doc.nodes[target_id];

        // Step 1: <picture><source> walk.
        if let Some(parent_id) = node.parent_id() {
            let parent = &self.doc.nodes[parent_id];
            if parent
                .data
                .is_element_with_tag_name(&local_name!("picture"))
            {
                for sibling_id in parent.children() {
                    if sibling_id == target_id {
                        // <source> elements only count if they
                        // precede the <img> in tree order.
                        break;
                    }
                    let sibling = &self.doc.nodes[sibling_id];
                    if !sibling
                        .data
                        .is_element_with_tag_name(&local_name!("source"))
                    {
                        continue;
                    }
                    // Unknown and disabled formats must not shadow a later supported source.
                    if let Some(ty) = sibling.attr(local_name!("type"))
                        && !Self::is_supported_picture_source_type(ty)
                    {
                        continue;
                    }
                    if let Some(srcset) = sibling.attr(local_name!("srcset"))
                        && let Some(candidate) = pick_srcset_candidate(srcset)
                    {
                        return Some(candidate);
                    }
                }
            }
        }

        // Step 2: <img srcset>.
        if let Some(srcset) = node.attr(local_name!("srcset"))
            && let Some(candidate) = pick_srcset_candidate(srcset)
        {
            return Some(candidate);
        }

        // Step 3: <img src> fallback.
        node.attr(local_name!("src"))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Read the `<img width|height>` attrs as a hint for the returned raster's
    /// size cap. CSS sizing is unavailable because image loading starts before
    /// the cascade runs.
    fn read_img_display_dim(&self, node_id: NodeId) -> Option<u32> {
        let node = &self.doc.nodes[node_id];
        let w = node
            .attr(local_name!("width"))
            .and_then(|s| s.trim().parse::<u32>().ok());
        let h = node
            .attr(local_name!("height"))
            .and_then(|s| s.trim().parse::<u32>().ok());
        match (w, h) {
            (Some(w), Some(h)) => Some(w.max(h)),
            (Some(w), None) => Some(w),
            (None, Some(h)) => Some(h),
            (None, None) => None,
        }
    }

    /// Load the image of an `<img>` (its `<picture>` source, `srcset` or `src`), or the
    /// href of an SVG `<image>` or `<feImage>`, which usvg reads from the image cache
    /// when parsing the `<svg>`.
    fn load_image(&mut self, target_id: NodeId) {
        self.doc.sync_base_url();
        // An <img> waits on its latest request only. Changing
        // src left the node queued under the previous URL too, so
        // whichever response landed first won: a stale success painted
        // the old image, and a stale 404 reported the node broken (the
        // shim keeps the first terminal state), swallowing the real
        // load event (WPT img.complete.html "async src complete test").
        // The old fetch stays in flight and still fills the cache.
        for waiting in self.doc.pending_images.values_mut() {
            waiting.retain(|(id, ty)| !(*id == target_id && matches!(ty, ImageType::Image)));
        }
        if self.doc.nodes[target_id]
            .parent_id()
            .is_some_and(|parent_id| is_template_inert_context(self.doc, parent_id))
        {
            return;
        }
        let Some(element) = self.doc.nodes[target_id].element_data() else {
            return;
        };
        let is_svg = element.ns() == ns!(svg);
        let raw_src = if is_svg {
            // usvg resolves `data:` URLs and `#fragment` references itself
            element
                .svg_href()
                .filter(|href| !href.starts_with('#') && !is_data_url(href))
                .map(str::to_string)
        } else {
            self.select_image_url(target_id)
        };
        let Some(raw_src) = raw_src.filter(|src| !src.is_empty()) else {
            return;
        };
        // An unresolvable src (`img.src = "http://"`) leaves the element broken;
        // the embedder fires the error event, so just skip the fetch.
        let Some(src) = self.doc.resolve_url(&raw_src) else {
            return;
        };
        let display_dim = if is_svg {
            None
        } else {
            self.read_img_display_dim(target_id)
        };

        // Check cache first
        if let Some(cached_image) = self.doc.image_cache.get(src.as_str()) {
            #[cfg(feature = "tracing")]
            tracing::info!("Loading image {src} from cache");
            let node = &mut self.doc.nodes[target_id];
            node.element_data_mut()
                .unwrap()
                .set_special_data(SpecialElementData::Image(Box::new(cached_image.clone())));
            node.clear_layout_cache();
            node.insert_damage(ALL_DAMAGE);
            return;
        }

        self.doc
            .fetch_image(src, target_id, ImageType::Image, display_dim);
    }

    fn load_iframe(&mut self, target_id: NodeId) {
        self.doc.sync_base_url();
        if self.doc.subdocument_depth >= crate::iframe::MAX_SUBDOCUMENT_DEPTH {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Not loading iframe: max sub-document nesting depth ({}) reached",
                crate::iframe::MAX_SUBDOCUMENT_DEPTH
            );
            return;
        }

        let node = &self.doc.nodes[target_id];
        let Some(element) = node.element_data() else {
            return;
        };

        // `srcdoc` takes precedence over `src`
        if let Some(srcdoc) = element.attr(local_name!("srcdoc")) {
            let srcdoc = srcdoc.to_string();
            self.doc.load_iframe_srcdoc(target_id, &srcdoc);
            return;
        }

        let Some(raw_src) = element.attr(local_name!("src")) else {
            return;
        };
        if raw_src.is_empty() {
            return;
        }
        let Some(url) = self.doc.document_base_url.resolve_relative(raw_src) else {
            #[cfg(feature = "tracing")]
            tracing::warn!("Not loading iframe: could not resolve url {raw_src}");
            return;
        };
        self.doc.start_iframe_load(target_id, url);
    }

    fn load_custom_paint_src(&mut self, target_id: NodeId) {
        let node = &mut self.doc.nodes[target_id];
        if let Some(raw_src) = node.attr(local_name!("src")) {
            if let Ok(custom_paint_source_id) = raw_src.parse::<u64>() {
                self.recompute_is_animating = true;
                let canvas_data = SpecialElementData::Canvas(Box::new(CanvasData {
                    custom_paint_source_id,
                }));
                node.element_data_mut()
                    .unwrap()
                    .set_special_data(canvas_data);
            }
        }
    }

    /// Append/update/remove the synthetic Text child
    /// that paints as the closed-`<select>` label. No-op for non-
    /// `<select>` ids (form_nodes also tracks button/input/etc.).
    ///
    /// `text = None` means "no label" (empty select, or no option
    /// has a non-empty visible text); the prior label is dropped
    /// if present. `text = Some(s)` updates an existing label in
    /// place (last Text child of the select) via set_node_text,
    /// or appends a fresh one. Idempotent: re-runs converge on
    /// the right state.
    ///
    /// Used by two callers:
    /// - The parse-time pass in `flush()` walks form_nodes,
    ///   resolves the initially-selected option from the wasm
    ///   `selected` content attribute, and calls this with the
    ///   resolved text.
    /// - The runtime resync path called from the dom-shim (which
    ///   tracks IDL-level selectedness in a JS Map, not in the
    ///   `selected` content attribute per spec). The JS side
    ///   resolves the label itself and hands the resolved text
    ///   to the wasm; we can't re-derive it here because the
    ///   wasm-visible attribute would lie about the live state.
    pub fn set_select_label(&mut self, target_id: NodeId, text: Option<&str>) {
        let Some(node) = self.doc.get_node(target_id) else {
            return;
        };
        let Some(data) = node.element_data() else {
            return;
        };
        if &*data.name.local != "select" {
            return;
        }

        // Last child being a Text node is the marker for a
        // previously-injected label. <select>'s content model is
        // option / optgroup / script / template only, so any
        // Text child can only be ours (or parser-emitted
        // whitespace, which is invisible either way).
        let existing_label = node
            .children()
            .last()
            .filter(|&id| self.doc.nodes[id].is_text_node());

        let normalized = text.filter(|s| !s.is_empty());

        match (existing_label, normalized) {
            (Some(text_id), Some(s)) => self.set_node_text(text_id, s),
            (Some(text_id), None) => {
                self.remove_node(text_id);
            }
            (None, Some(s)) => {
                let text_id = self.create_text_node(s);
                self.append_children(target_id, &[text_id]);
            }
            (None, None) => {}
        }
    }

    /// Seed a closed-select label from content attributes in tree order,
    /// including options inside `<optgroup>`. Non-selects return without a label.
    pub fn sync_select_label_from_attrs(&mut self, target_id: NodeId) {
        let Some(node) = self.doc.get_node(target_id) else {
            return;
        };
        let Some(data) = node.element_data() else {
            return;
        };
        if &*data.name.local != "select" {
            return;
        }

        let multiple = data.attr(local_name!("multiple")).is_some();
        let display_size = data
            .attr(local_name!("size"))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let select_first_enabled_by_default = !multiple && display_size <= 1;
        let mut chosen: Option<NodeId> = None;
        let mut first_option: Option<NodeId> = None;
        let mut first_enabled_option: Option<NodeId> = None;
        let mut pending = node
            .children()
            .iter()
            .rev()
            .map(|id| (id, false))
            .collect::<Vec<_>>();
        while let Some((child_id, disabled_by_group)) = pending.pop() {
            let child_node = &self.doc.nodes[child_id];
            let Some(child_data) = child_node.element_data() else {
                continue;
            };
            if &*child_data.name.local == "option" {
                if first_option.is_none() {
                    first_option = Some(child_id);
                }
                let disabled =
                    disabled_by_group || child_data.attr(local_name!("disabled")).is_some();
                if !disabled && first_enabled_option.is_none() {
                    first_enabled_option = Some(child_id);
                }
                if child_data.attr(local_name!("selected")).is_some() {
                    if multiple {
                        if chosen.is_none() {
                            chosen = Some(child_id);
                        }
                    } else {
                        chosen = Some(child_id);
                    }
                }
            } else if &*child_data.name.local == "optgroup" {
                let group_disabled =
                    disabled_by_group || child_data.attr(local_name!("disabled")).is_some();
                pending.extend(
                    child_node
                        .children()
                        .iter()
                        .rev()
                        .map(|id| (id, group_disabled)),
                );
            }
        }

        let default_option = if select_first_enabled_by_default {
            first_enabled_option
        } else {
            None
        };
        let text = chosen
            .or(default_option)
            .or(first_option)
            .map(|opt_id| self.doc.nodes[opt_id].text_content());
        self.set_select_label(target_id, text.as_deref());
    }

    /// JS-side `input.value = ...` / `textarea.value = ...`
    /// IDL setter. Updates the text input's live editor buffer
    /// without writing the `value` content attribute: the IDL
    /// value and the content attribute are decoupled per HTML (the
    /// attribute reflects defaultValue, not the live state).
    /// Mirroring the IDL set into setAttribute("value", ...) to
    /// trigger the same input_data.set_text repaint would make
    /// getAttribute("value") lie about the initial value. A
    /// dedicated entry keeps them separate, mirroring the set_checkbox_checked precedent.
    ///
    /// Returns false when the node isn't a text-input-bearing
    /// element (caller can fall back to the attribute mirror).
    pub fn set_text_input_value(&mut self, target_id: NodeId, value: &str) -> bool {
        // Snapshot + damage first, same as set_attribute's
        // in-document path: the editor buffer feeds layout, and
        // stylo's invalidator needs the snapshot to restyle.
        let Some(node) = self.doc.get_node(target_id) else {
            return false;
        };
        let node_is_in_document = node.flags.is_in_document();
        if node_is_in_document {
            self.doc.snapshot_node(target_id, &local_name!("value"));
            let node = &mut self.doc.nodes[target_id];
            if let Some(mut data) = node
                .try_stylo_element_data_mut()
                .and_then(|stylo| stylo.get_mut())
            {
                data.hint |= RestyleHint::restyle_subtree();
                data.damage.insert(ALL_DAMAGE);
            }
            self.doc.nodes[target_id].mark_ancestors_dirty();
        }

        let node = &mut self.doc.nodes[target_id];
        let NodeData::Element(ref mut element) = node.data else {
            return false;
        };
        // A .value write can land before the first layout
        // pass has run create_text_editor (script during parse):
        // there is no editor yet and the write would be dropped.
        // Seed one so construct's update branch picks it up.
        if element.text_input_data_mut().is_none() {
            let tag: &str = &element.name.local;
            let is_multiline = tag == "textarea";
            // Mirror construct's gate: ASCII-ci type matching with
            // unknown/empty falling to the Text state per HTML.
            let ty = element
                .attr(local_name!("type"))
                .map(|t| t.to_ascii_lowercase());
            let is_text_input = tag == "input"
                && !matches!(
                    ty.as_deref(),
                    Some(
                        "checkbox"
                            | "radio"
                            | "range"
                            | "hidden"
                            | "submit"
                            | "button"
                            | "reset"
                            | "file"
                            | "image"
                            | "color"
                            | "date"
                            | "datetime-local"
                            | "month"
                            | "week"
                            | "time"
                    )
                );
            if !(is_multiline || is_text_input) {
                return false;
            }
            element.set_special_data(SpecialElementData::TextInput(Box::new(TextInputData::new(
                is_multiline,
            ))));
        }
        let Some(input_data) = element.text_input_data_mut() else {
            return false;
        };
        // Record the override before set_text: layout construction
        // re-derives the display text on every pass and would
        // otherwise clobber the editor back to the attribute value.
        input_data.value_override = Some(value.to_string());
        input_data.set_text(
            &mut self.doc.font_ctx.lock().unwrap(),
            &mut self.doc.layout_ctx,
            value,
        );
        true
    }

    /// JS-side text-selection sync. Maps the HTML
    /// selectionStart/selectionEnd (UTF-16 code-unit offsets) onto
    /// the parley editor's byte-offset selection so the paint layer
    /// (which reads editor.selection_geometry / cursor_geometry for
    /// focused nodes) renders the caret and highlight the shim's
    /// Selection API state describes.
    pub fn set_text_input_selection(
        &mut self,
        target_id: NodeId,
        start_u16: usize,
        end_u16: usize,
    ) -> bool {
        if self.doc.get_node(target_id).is_none() {
            return false;
        }
        let node = &mut self.doc.nodes[target_id];
        let NodeData::Element(ref mut element) = node.data else {
            return false;
        };
        let Some(input_data) = element.text_input_data_mut() else {
            return false;
        };
        // UTF-16 code units → byte offsets. Inputs are small; the
        // copy dodges the editor borrow across the driver call.
        let text: String = input_data.editor.raw_text().to_string();
        let to_byte = |cu: usize| -> usize {
            let mut count = 0usize;
            for (byte_idx, ch) in text.char_indices() {
                if count >= cu {
                    return byte_idx;
                }
                count += ch.len_utf16();
            }
            text.len()
        };
        let (sb, eb) = (to_byte(start_u16), to_byte(end_u16));
        let mut font_ctx = self.doc.font_ctx.lock().unwrap();
        let mut driver = input_data
            .editor
            .driver(&mut font_ctx, &mut self.doc.layout_ctx);
        if sb == eb {
            driver.move_to_byte(sb);
        } else {
            driver.select_byte_range(sb, eb);
        }
        true
    }

    fn process_button_input(&mut self, target_id: NodeId) {
        let node = &self.doc.nodes[target_id];
        let Some(data) = node.element_data() else {
            return;
        };

        let tagname = data.name.local.as_ref();
        let type_attr = data.attr(local_name!("type"));
        let value = data.attr(local_name!("value"));

        // Checkbox state must exist even if the input never gets a box (e.g. `display: none`
        // inside a `<label>`), so that it can be toggled and matched by `:checked`.
        if let ("input", Some("checkbox" | "radio")) = (tagname, type_attr) {
            let element = self.doc.nodes[target_id].element_data_mut().unwrap();
            if element.checkbox_input_checked().is_none() {
                let checked = element.has_attr(local_name!("checked"));
                element.set_special_data(SpecialElementData::CheckboxInput(checked));
            }
            return;
        }

        // Add content of "value" attribute as a text node child if:
        //   - Tag name is
        if let ("input", Some("button" | "submit" | "reset"), Some(value)) =
            (tagname, type_attr, value)
        {
            let value = value.to_string();
            let id = self.create_text_node(&value);
            self.append_children(target_id, &[id]);
            return;
        }

        // <select> label injection is deliberately
        // not handled here. The HTML parser calls
        // process_added_subtree (which queues this fn) before
        // children are attached, so we'd see an empty option
        // list every time. Instead, sync_select_labels() runs
        // from `flush()` at end-of-parse against the
        // accumulated `form_nodes` set: by then the option
        // children are fully populated. See the comment at
        // sync_select_label below for the design rationale.
        #[cfg(feature = "file-input")]
        if let ("input", Some("file")) = (tagname, type_attr) {
            let button_id = self.create_element(
                qual_name!("button", html),
                vec![
                    Attribute::new(qual_name!("type", html), "button".to_string()),
                    Attribute::new(qual_name!("tabindex", html), "-1".to_string()),
                ],
            );
            let label_id = self.create_element(qual_name!("label", html), vec![]);
            let text_id = self.create_text_node("No File Selected");
            let button_text_id = self.create_text_node("Browse");
            self.append_children(target_id, &[button_id, label_id]);
            self.append_children(label_id, &[text_id]);
            self.append_children(button_id, &[button_text_id]);
        }
    }
}

/// Set 'checked' state on an input based on given attributevalue
fn is_data_url(url: &str) -> bool {
    url.trim_start()
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
}

fn set_input_checked_state(element: &mut ElementData, value: String) {
    let Ok(checked) = value.parse() else {
        return;
    };
    match element.special_data() {
        SpecialElementData::CheckboxInput(_) => element.set_checkbox_input_checked(checked),
        // If we have just constructed the element, set the node attribute,
        // and NodeSpecificData will be created from that later
        // this simulates the checked attribute being set in html,
        // and the element's checked property being set from that
        SpecialElementData::None => element.attrs.push(Attribute::new(
            qual_name!("checked", html),
            checked.to_string(),
        )),
        _ => {}
    }
}

/// Type that allows mutable access to the viewport
/// And syncs it back to stylist on drop.
pub struct ViewportMut<'doc> {
    doc: &'doc mut BaseDocument,
    initial_viewport: Viewport,
}
impl ViewportMut<'_> {
    pub fn new(doc: &mut BaseDocument) -> ViewportMut<'_> {
        let initial_viewport = doc.viewport.clone();
        ViewportMut {
            doc,
            initial_viewport,
        }
    }
}
impl Deref for ViewportMut<'_> {
    type Target = Viewport;

    fn deref(&self) -> &Self::Target {
        &self.doc.viewport
    }
}
impl DerefMut for ViewportMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.doc.viewport
    }
}
impl Drop for ViewportMut<'_> {
    fn drop(&mut self) {
        if self.doc.viewport == self.initial_viewport {
            return;
        }

        let changes = DeviceChanges::from_viewports(&self.initial_viewport, &self.doc.viewport);
        self.doc.queue_device_changes(changes);
    }
}

/// Pick a URL from an HTML `srcset` attribute.
///
#[derive(Clone, Copy)]
enum SrcsetDescriptor<'a> {
    Width(&'a str),
    Density(f64),
}

// Keep arbitrary-length widths normalized as decimal text so comparisons cannot overflow.
fn parse_srcset_positive_integer(value: &str) -> Option<&str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = value.trim_start_matches('0');
    (!value.is_empty()).then_some(value)
}

// Check the HTML number syntax first; Rust's float parser also accepts NaN and infinity.
fn parse_srcset_density(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut i = 0;
    if bytes
        .first()
        .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
    {
        i += 1;
    }
    let integer_start = i;
    while bytes.get(i).is_some_and(|byte| byte.is_ascii_digit()) {
        i += 1;
    }
    let mut has_digits = i > integer_start;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let fraction_start = i;
        while bytes.get(i).is_some_and(|byte| byte.is_ascii_digit()) {
            i += 1;
        }
        if i == fraction_start {
            return None;
        }
        has_digits = true;
    }
    if !has_digits {
        return None;
    }
    if bytes
        .get(i)
        .is_some_and(|byte| matches!(*byte, b'e' | b'E'))
    {
        i += 1;
        if bytes
            .get(i)
            .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
        {
            i += 1;
        }
        let exponent_start = i;
        while bytes.get(i).is_some_and(|byte| byte.is_ascii_digit()) {
            i += 1;
        }
        if i == exponent_start {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }

    let value = value.parse::<f64>().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

// The HTML height descriptor is valid only with width and does not affect width ranking.
fn parse_srcset_descriptor(descriptor: &str) -> Option<SrcsetDescriptor<'_>> {
    let mut width = None;
    let mut height = None;
    let mut density = None;

    for token in descriptor.split_ascii_whitespace() {
        if let Some(value) = token.strip_suffix('w') {
            let value = parse_srcset_positive_integer(value)?;
            if width.replace(value).is_some() {
                return None;
            }
        } else if let Some(value) = token.strip_suffix('h') {
            let value = parse_srcset_positive_integer(value)?;
            if height.replace(value).is_some() {
                return None;
            }
        } else {
            let value = token.strip_suffix('x')?;
            let value = parse_srcset_density(value)?;
            if density.replace(value).is_some() {
                return None;
            }
        }
    }

    match (width, height, density) {
        (Some(width), None, None) | (Some(width), Some(_), None) => {
            Some(SrcsetDescriptor::Width(width))
        }
        (None, None, Some(density)) => Some(SrcsetDescriptor::Density(density)),
        (None, None, None) => Some(SrcsetDescriptor::Density(1.0)),
        _ => None,
    }
}

/// Choose the largest candidate available to the headless renderer instead of
/// applying viewport-aware selection. Missing descriptors mean `1x`; invalid
/// candidates are skipped, and width descriptors win in mixed lists.
fn pick_srcset_candidate(srcset: &str) -> Option<String> {
    // Width descriptors take precedence over density descriptors when both
    // appear in one list; real pages sometimes provide this invalid mixture.
    let mut best: Option<(&str, SrcsetDescriptor)> = None;

    // Whitespace-anchored scan, not a `,`-split; `data:` URLs
    // contain literal commas (between `image/png;base64` and the
    // payload) and would shred on a naive split. WHATWG §srcset
    // parsing is character-by-character: skip leading whitespace,
    // read a URL (non-whitespace run), read an optional descriptor
    // run, then consume the comma separator. Commas at the end of
    // a URL token (i.e. directly attached, no whitespace) are
    // stripped per spec and act as separators in that one case.
    let bytes = srcset.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace and commas between entries.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read URL: run of non-whitespace.
        let url_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut url_end = i;
        // A trailing comma at the end of a URL acts as the entry
        // separator (per spec). Strip it off the URL token.
        let mut had_trailing_comma = false;
        if url_end > url_start && bytes[url_end - 1] == b',' {
            url_end -= 1;
            had_trailing_comma = true;
        }
        if url_end == url_start {
            continue;
        }
        let url = &srcset[url_start..url_end];

        // Read descriptor: skip whitespace, then read up to the
        // next comma (or end). Skip if we already consumed the
        // separator via a trailing comma on the URL.
        let descriptor = if had_trailing_comma {
            ""
        } else {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
                i += 1;
            }
            let desc_start = i;
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            srcset[desc_start..i].trim()
        };
        // Consume the comma separator if present; the top-of-loop
        // skip will eat any trailing whitespace.
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
        }

        let Some(descriptor) = parse_srcset_descriptor(descriptor) else {
            continue;
        };

        let take = match (&best, descriptor) {
            (None, _) => true,
            (Some((_, SrcsetDescriptor::Density(_))), SrcsetDescriptor::Width(_)) => true,
            (Some((_, SrcsetDescriptor::Width(_))), SrcsetDescriptor::Density(_)) => false,
            (Some((_, SrcsetDescriptor::Width(previous))), SrcsetDescriptor::Width(value)) => {
                (value.len(), value) > (previous.len(), *previous)
            }
            (Some((_, SrcsetDescriptor::Density(previous))), SrcsetDescriptor::Density(value)) => {
                value > *previous
            }
        };
        if take {
            best = Some((url, descriptor));
        }
    }

    best.map(|(url, _)| url.to_string())
}

#[cfg(test)]
mod test {
    use style::media_queries::MediaType;
    use style::servo_arc::Arc as ServoArc;
    use style_dom::ElementState;

    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport};

    use super::{DocumentMutator, NodeId};
    use crate::{
        Attribute, BaseDocument, DocumentConfig, ElementData, NodeData, UserAgentStylesheet,
        qual_name,
    };

    #[test]
    fn unresolvable_relative_urls_are_skipped() {
        // Without a base url, relative urls cannot be resolved against the default
        // `data:` document url. Loading such resources should be skipped, not panic.
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id();
        let attr = |name, value: &str| Attribute::new(name, value.to_string());

        let mut mutator = document.mutate();
        let html = mutator.create_element(qual_name!("html"), vec![]);
        let img = mutator.create_element(
            qual_name!("img"),
            vec![attr(qual_name!("src"), "image.png")],
        );
        let link = mutator.create_element(
            qual_name!("link"),
            vec![
                attr(qual_name!("rel"), "stylesheet"),
                attr(qual_name!("href"), "style.css"),
            ],
        );
        mutator.append_children(html, &[img, link]);
        mutator.append_children(root_id, &[html]);
        mutator.set_attribute(img, qual_name!("src"), "other.png");
        drop(mutator);

        document.resolve(0.0);
    }

    #[test]
    fn removing_href_from_a_link_without_a_loaded_stylesheet() {
        // An icon link never holds a stylesheet, and a stylesheet link holds none
        // until its fetch completes (here it never starts, as the url is unresolvable).
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id();
        let attr = |name, value: &str| Attribute::new(name, value.to_string());

        let mut mutator = document.mutate();
        let html = mutator.create_element(qual_name!("html"), vec![]);
        let icon = mutator.create_element(
            qual_name!("link"),
            vec![
                attr(qual_name!("rel"), "icon"),
                attr(qual_name!("href"), "favicon.png"),
            ],
        );
        let stylesheet = mutator.create_element(
            qual_name!("link"),
            vec![
                attr(qual_name!("rel"), "stylesheet"),
                attr(qual_name!("href"), "style.css"),
            ],
        );
        mutator.append_children(html, &[icon, stylesheet]);
        mutator.append_children(root_id, &[html]);
        mutator.clear_attribute(icon, qual_name!("href"));
        mutator.clear_attribute(stylesheet, qual_name!("href"));
        drop(mutator);

        document.resolve(0.0);
    }

    #[cfg(feature = "custom-widget")]
    #[test]
    fn custom_widget_replays_initial_attributes() {
        use std::sync::Mutex;

        type AttributeChange = (String, Option<String>, Option<String>);
        struct RecordingWidget(Arc<Mutex<Vec<AttributeChange>>>);

        impl crate::Widget for RecordingWidget {
            fn attribute_changed(&mut self, name: &str, old: Option<&str>, new: Option<&str>) {
                self.0.lock().unwrap().push((
                    name.to_owned(),
                    old.map(str::to_owned),
                    new.map(str::to_owned),
                ));
            }
        }

        for via_mutator in [false, true] {
            for initially_empty in [false, true] {
                let mut document = BaseDocument::new(DocumentConfig::default());
                let attrs = if initially_empty {
                    vec![]
                } else {
                    vec![
                        Attribute::new(qual_name!("title"), "initial"),
                        Attribute::new(qual_name!("lang"), ""),
                    ]
                };
                let id = document.create_node(NodeData::Element(crate::pool::PoolBox::new(
                    ElementData::new(qual_name!("div"), attrs),
                )));
                let calls = Arc::new(Mutex::new(Vec::new()));
                let widget = Box::new(RecordingWidget(Arc::clone(&calls)));
                if via_mutator {
                    document.mutate().set_custom_widget(id, widget);
                } else {
                    document.set_custom_widget(id, widget);
                }

                let mut expected = if initially_empty {
                    vec![]
                } else {
                    vec![
                        ("title".into(), None, Some("initial".into())),
                        ("lang".into(), None, Some("".into())),
                    ]
                };
                assert_eq!(*calls.lock().unwrap(), expected);

                let mut mutator = document.mutate();
                mutator.set_attribute(id, qual_name!("title"), "updated");
                mutator.clear_attribute(id, qual_name!("title"));
                expected.extend([
                    (
                        "title".into(),
                        (!initially_empty).then(|| "initial".into()),
                        Some("updated".into()),
                    ),
                    ("title".into(), Some("updated".into()), None),
                ]);
                assert_eq!(*calls.lock().unwrap(), expected);
            }
        }
    }

    #[test]
    fn srcset_single_url_no_descriptor() {
        assert_eq!(
            super::pick_srcset_candidate("foo.jpg").as_deref(),
            Some("foo.jpg")
        );
    }

    #[test]
    fn srcset_picks_largest_width() {
        assert_eq!(
            super::pick_srcset_candidate("a.jpg 480w, b.jpg 960w, c.jpg 240w").as_deref(),
            Some("b.jpg")
        );
    }

    #[test]
    fn srcset_picks_largest_density() {
        assert_eq!(
            super::pick_srcset_candidate("a.png 1x, b.png 2x, c.png 3x").as_deref(),
            Some("c.png")
        );
    }

    #[test]
    fn srcset_width_beats_density_when_mixed() {
        // Spec-invalid but seen in the wild. Widths win because
        // they give us higher-res source bytes.
        assert_eq!(
            super::pick_srcset_candidate("small.jpg 2x, big.jpg 800w").as_deref(),
            Some("big.jpg")
        );
    }

    #[test]
    fn srcset_skips_empty_entries() {
        assert_eq!(
            super::pick_srcset_candidate(" , foo.jpg 100w , ").as_deref(),
            Some("foo.jpg")
        );
    }

    #[test]
    fn srcset_empty_returns_none() {
        assert!(super::pick_srcset_candidate("").is_none());
        assert!(super::pick_srcset_candidate("  ,  ,  ").is_none());
    }

    #[test]
    fn srcset_rejects_invalid_descriptors() {
        for srcset in [
            "zero-width.jpg 0w",
            "negative-width.jpg -1w",
            "fractional-width.jpg 1.5w",
            "not-finite-width.jpg NaNw",
            "trailing-decimal-point.jpg 1.x",
            "zero-density.jpg 0x",
            "negative-density.jpg -1x",
            "not-finite-density.jpg Infinityx",
            "not-a-number-density.jpg NaNx",
            "invalid-exponent.jpg 1e+x",
            "multiple-descriptors.jpg 1x 2x",
            "unknown-descriptor.jpg nope",
            "zero-height.jpg 320w 0h",
        ] {
            assert!(
                super::pick_srcset_candidate(srcset).is_none(),
                "invalid candidate must be rejected: {srcset}"
            );
        }
    }

    #[test]
    fn srcset_accepts_valid_float_density_descriptors() {
        assert_eq!(
            super::pick_srcset_candidate("fraction.png 0.5x, decimal.png 1.25x, exponent.png 2e0x")
                .as_deref(),
            Some("exponent.png")
        );
    }

    #[test]
    fn srcset_orders_arbitrarily_large_width_descriptors() {
        assert_eq!(
            super::pick_srcset_candidate(
                "small.jpg 18446744073709551616w, large.jpg 340282366920938463463374607431768211456w"
            )
            .as_deref(),
            Some("large.jpg")
        );
    }

    #[test]
    fn srcset_skips_invalid_candidates() {
        assert_eq!(
            super::pick_srcset_candidate("invalid.jpg NaNw, valid.jpg 640w").as_deref(),
            Some("valid.jpg")
        );
    }

    #[test]
    fn srcset_accepts_width_and_height_descriptors() {
        assert_eq!(
            super::pick_srcset_candidate("small.jpg 320w 200h, large.jpg 640w 400h").as_deref(),
            Some("large.jpg")
        );
    }

    #[test]
    fn srcset_data_url_with_commas() {
        // data: URLs contain a comma between the mime-prefix and
        // the payload. A naive `,`-split would shred the URL into
        // pieces; the whitespace-anchored scanner has to keep it
        // intact.
        let s = "data:image/png;base64,iVBORw0KGgo= 100w, https://x/big.png 400w";
        assert_eq!(
            super::pick_srcset_candidate(s).as_deref(),
            Some("https://x/big.png")
        );
    }

    #[test]
    fn srcset_data_url_only_no_descriptor() {
        let s = "data:image/png;base64,iVBORw0KGgo=";
        assert_eq!(super::pick_srcset_candidate(s).as_deref(), Some(s));
    }

    fn make_picture_doc(sources: &[(&str, &str)], img_src: &str) -> (BaseDocument, NodeId) {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id();
        let img_id = {
            let mut mutator = document.mutate();
            let picture = mutator.create_element(qual_name!("picture"), vec![]);
            let mut children = Vec::with_capacity(sources.len() + 1);
            for &(type_hint, srcset) in sources {
                children.push(mutator.create_element(
                    qual_name!("source"),
                    vec![
                        Attribute::new(qual_name!("type"), type_hint),
                        Attribute::new(qual_name!("srcset"), srcset),
                    ],
                ));
            }
            let img = mutator.create_element(
                qual_name!("img"),
                vec![Attribute::new(qual_name!("src"), img_src)],
            );
            children.push(img);
            mutator.append_children(picture, &children);
            mutator.append_children(root_id, &[picture]);
            img
        };
        (document, img_id)
    }

    #[test]
    fn picture_skips_unsupported_mime_sources() {
        let (mut document, img_id) = make_picture_doc(
            &[
                ("image/avif; codecs=av01", "unsupported.avif 2x"),
                ("image/jxl", "unsupported.jxl 2x"),
                (" IMAGE/WEBP ; codecs=vp8", "supported.webp 2x"),
            ],
            "fallback.png",
        );
        let mutator = document.mutate();
        assert_eq!(
            mutator.select_image_url(img_id).as_deref(),
            Some("supported.webp")
        );
    }

    #[test]
    fn picture_accepts_empty_and_legacy_source_types() {
        for type_hint in [
            "",
            " ",
            "image/jpg",
            "image/pjpeg",
            "image/x-png",
            "image/apng",
        ] {
            let (mut document, img_id) =
                make_picture_doc(&[(type_hint, "source.img 2x")], "fallback.png");
            let mutator = document.mutate();
            assert_eq!(
                mutator.select_image_url(img_id).as_deref(),
                Some("source.img"),
                "type={type_hint:?}"
            );
        }
    }

    #[test]
    fn picture_uses_img_src_when_all_source_types_are_unsupported() {
        let (mut document, img_id) =
            make_picture_doc(&[("image/jxl", "unsupported.jxl 2x")], "fallback.png");
        let mutator = document.mutate();
        assert_eq!(
            mutator.select_image_url(img_id).as_deref(),
            Some("fallback.png")
        );
    }

    fn select_label_text(document: &BaseDocument, select_id: NodeId) -> String {
        let label_id = document.nodes[select_id].children().last().unwrap();
        document.nodes[label_id].text_content()
    }

    #[test]
    fn select_label_follows_tree_order_and_default_selectedness() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id();
        let (select_id, selected_group_id, selected_later_id) = {
            let mut mutator = document.mutate();
            let select = mutator.create_element(qual_name!("select"), vec![]);
            let disabled_first = mutator.create_element(
                qual_name!("option"),
                vec![Attribute::new(qual_name!("disabled"), String::new())],
            );
            let disabled_first_text = mutator.create_text_node("Disabled first");
            mutator.append_children(disabled_first, &[disabled_first_text]);

            let disabled_group = mutator.create_element(
                qual_name!("optgroup"),
                vec![Attribute::new(qual_name!("disabled"), String::new())],
            );
            let disabled_group_option = mutator.create_element(qual_name!("option"), vec![]);
            let disabled_group_text = mutator.create_text_node("Disabled group");
            mutator.append_children(disabled_group_option, &[disabled_group_text]);
            mutator.append_children(disabled_group, &[disabled_group_option]);

            let group = mutator.create_element(qual_name!("optgroup"), vec![]);
            let first_enabled = mutator.create_element(qual_name!("option"), vec![]);
            let first_enabled_text = mutator.create_text_node("First enabled");
            mutator.append_children(first_enabled, &[first_enabled_text]);
            let selected_group = mutator.create_element(
                qual_name!("option"),
                vec![Attribute::new(qual_name!("selected"), String::new())],
            );
            let selected_group_text = mutator.create_text_node("Selected group");
            mutator.append_children(selected_group, &[selected_group_text]);
            mutator.append_children(group, &[first_enabled, selected_group]);

            let selected_later = mutator.create_element(
                qual_name!("option"),
                vec![Attribute::new(qual_name!("selected"), String::new())],
            );
            let selected_later_text = mutator.create_text_node("Selected later");
            mutator.append_children(selected_later, &[selected_later_text]);

            mutator.append_children(
                select,
                &[disabled_first, disabled_group, group, selected_later],
            );
            mutator.append_children(root_id, &[select]);
            (select, selected_group, selected_later)
        };

        document.mutate().sync_select_label_from_attrs(select_id);
        assert_eq!(select_label_text(&document, select_id), "Selected later");

        document
            .mutate()
            .clear_attribute(selected_later_id, qual_name!("selected"));
        document.mutate().sync_select_label_from_attrs(select_id);
        assert_eq!(select_label_text(&document, select_id), "Selected group");

        document
            .mutate()
            .clear_attribute(selected_group_id, qual_name!("selected"));
        document.mutate().sync_select_label_from_attrs(select_id);
        assert_eq!(select_label_text(&document, select_id), "First enabled");

        document
            .mutate()
            .set_attribute(select_id, qual_name!("multiple"), "");
        document
            .mutate()
            .set_attribute(selected_group_id, qual_name!("selected"), "");
        document
            .mutate()
            .set_attribute(selected_later_id, qual_name!("selected"), "");
        document.mutate().sync_select_label_from_attrs(select_id);
        assert_eq!(select_label_text(&document, select_id), "Selected group");

        document
            .mutate()
            .clear_attribute(selected_group_id, qual_name!("selected"));
        document
            .mutate()
            .clear_attribute(selected_later_id, qual_name!("selected"));
        document.mutate().sync_select_label_from_attrs(select_id);
        assert_eq!(select_label_text(&document, select_id), "Disabled first");
    }

    #[test]
    fn media_type_defaults_to_screen() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        assert_eq!(*document.media_type(), MediaType::screen());
        assert_eq!(document.stylist_device().media_type(), MediaType::screen());
    }

    #[test]
    fn media_type_honors_config() {
        let mut document = BaseDocument::new(DocumentConfig {
            media_type: Some(MediaType::print()),
            ..Default::default()
        });
        assert_eq!(*document.media_type(), MediaType::print());
        assert_eq!(document.stylist_device().media_type(), MediaType::print());
    }

    #[test]
    fn set_media_type_updates_stylist_device() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        assert_eq!(document.stylist_device().media_type(), MediaType::screen());

        document.set_media_type(MediaType::print());
        assert_eq!(*document.media_type(), MediaType::print());
        assert_eq!(document.stylist_device().media_type(), MediaType::print());
    }

    #[test]
    fn mutator_remove_disabled() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let id = document.create_node(NodeData::Element(crate::pool::PoolBox::new(
            ElementData::new(
                qual_name!("button"),
                vec![Attribute::new(qual_name!("disabled"), "")],
            ),
        )));

        let node = document.get_node(id).unwrap();
        assert!(
            node.element_state().contains(ElementState::DISABLED),
            "form node is disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is not enabled yet"
        );

        let mut mutator = document.mutate();
        mutator.clear_attribute(id, qual_name!("disabled"));
        drop(mutator);

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is no longer disabled"
        );
        assert!(
            node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );
    }

    #[test]
    fn base_epoch_moves_only_for_connected_base_changes() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let base = document.create_node(NodeData::Element(crate::pool::PoolBox::new(
            ElementData::new(qual_name!("base"), vec![]),
        )));
        let div = document.create_node(NodeData::Element(crate::pool::PoolBox::new(
            ElementData::new(qual_name!("div"), vec![]),
        )));
        let root = document.root_node().id();
        let mut mutator = document.mutate();
        mutator.set_attribute(base, qual_name!("href"), "/detached/");
        drop(mutator);
        assert_eq!(document.base_epoch, 0, "a detached base does not count");

        let mut mutator = document.mutate();
        mutator.append_children(root, &[base, div]);
        drop(mutator);
        let attached = document.base_epoch;
        assert_ne!(attached, 0, "connecting a base moves the epoch");

        let mut mutator = document.mutate();
        mutator.set_attribute(div, qual_name!("href"), "/x/");
        mutator.set_attribute(base, qual_name!("target"), "_top");
        drop(mutator);
        assert_eq!(document.base_epoch, attached, "other writes leave it");

        let mut mutator = document.mutate();
        mutator.set_attribute(base, qual_name!("href"), "/y/");
        drop(mutator);
        let rewritten = document.base_epoch;
        assert_ne!(rewritten, attached, "a base href write moves it");

        let mut mutator = document.mutate();
        mutator.clear_attribute(base, qual_name!("href"));
        drop(mutator);
        assert_ne!(document.base_epoch, rewritten, "removing the href moves it");
    }

    #[test]
    fn resource_url_follows_the_first_base_href() {
        let mut document = BaseDocument::new(DocumentConfig {
            base_url: Some("https://doc.example/a/page.html".into()),
            ..Default::default()
        });
        let element = |document: &mut BaseDocument, name, href: Option<&str>| {
            let attrs = href.map_or_else(Vec::new, |href| {
                vec![Attribute::new(qual_name!("href"), href)]
            });
            document.create_node(NodeData::Element(crate::pool::PoolBox::new(
                ElementData::new(name, attrs),
            )))
        };
        let template = element(&mut document, qual_name!("template", html), None);
        let inert_href = Some("https://inert.example/");
        let inert = element(&mut document, qual_name!("base", html), inert_href);
        let empty = element(&mut document, qual_name!("base", html), None);
        let first = element(&mut document, qual_name!("base", html), Some("/b/"));
        let second_href = Some("https://second.example/");
        let second = element(&mut document, qual_name!("base", html), second_href);
        let root = document.root_node().id();
        let resolved = |document: &mut BaseDocument| {
            document.sync_base_url();
            document.resolve_url("s.css").unwrap().to_string()
        };
        assert_eq!(resolved(&mut document), "https://doc.example/a/s.css");

        let mut mutator = document.mutate();
        mutator.append_children(template, &[inert]);
        mutator.append_children(root, &[template, empty, first, second]);
        drop(mutator);
        assert_eq!(resolved(&mut document), "https://doc.example/b/s.css");

        assert_eq!(document.url().as_str(), "https://doc.example/a/page.html");

        for href in ["data:,", "data:/c/", "javascript:/c/", "https://[::1"] {
            let mut mutator = document.mutate();
            mutator.set_attribute(first, qual_name!("href"), href);
            drop(mutator);
            let unusable = resolved(&mut document);
            assert_eq!(unusable, "https://doc.example/a/s.css", "{href} falls back");
        }

        let mut mutator = document.mutate();
        mutator.remove_node(first);
        drop(mutator);
        assert_eq!(resolved(&mut document), "https://second.example/s.css");

        let mut mutator = document.mutate();
        mutator.remove_node(second);
        drop(mutator);
        assert_eq!(resolved(&mut document), "https://doc.example/a/s.css");
        assert_eq!(
            document.connected_bases.len(),
            1,
            "a <base> without href stays tracked"
        );

        let mut mutator = document.mutate();
        mutator.remove_node(empty);
        drop(mutator);
        assert!(
            document.connected_bases.is_empty(),
            "removed bases stop being tracked"
        );
        assert_eq!(resolved(&mut document), "https://doc.example/a/s.css");
    }

    #[test]
    fn linked_stylesheet_keeps_the_url_it_was_requested_with() {
        let mut document = BaseDocument::new(DocumentConfig {
            base_url: Some("https://doc.example/a/page.html".into()),
            ..Default::default()
        });
        let element = |document: &mut BaseDocument, name, attrs: Vec<Attribute>| {
            document.create_node(NodeData::Element(crate::pool::PoolBox::new(
                ElementData::new(name, attrs),
            )))
        };
        let base_attrs = vec![Attribute::new(qual_name!("href"), "/b/")];
        let base = element(&mut document, qual_name!("base", html), base_attrs);
        let link_attrs = vec![
            Attribute::new(qual_name!("rel"), "stylesheet"),
            Attribute::new(qual_name!("href"), "s.css"),
        ];
        let link = element(&mut document, qual_name!("link", html), link_attrs);
        let root = document.root_node().id();

        let mut mutator = document.mutate();
        mutator.append_children(root, &[base, link]);
        drop(mutator);
        let requested = document.linked_stylesheet_url(link).map(str::to_string);
        assert_eq!(requested.as_deref(), Some("https://doc.example/b/s.css"));

        let mut mutator = document.mutate();
        mutator.set_attribute(base, qual_name!("href"), "/c/");
        drop(mutator);
        let after_base_write = document.linked_stylesheet_url(link).map(str::to_string);
        assert_eq!(
            after_base_write, requested,
            "a base change does not refetch the sheet"
        );

        let mut mutator = document.mutate();
        mutator.remove_node(link);
        drop(mutator);
        assert_eq!(document.linked_stylesheet_url(link), None);
    }

    #[test]
    fn mutator_set_disabled() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let id = document.create_node(NodeData::Element(crate::pool::PoolBox::new(
            ElementData::new(qual_name!("button"), vec![]),
        )));

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is not disabled"
        );
        assert!(
            node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );

        let mut mutator = document.mutate();
        mutator.set_attribute(id, qual_name!("disabled"), "");
        drop(mutator);

        let node = document.get_node(id).unwrap();

        assert!(
            node.element_state().contains(ElementState::DISABLED),
            "form node is disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is no longer enabled enabled"
        );
    }

    #[test]
    fn mutator_set_disabled_invalid_node() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let id = document.create_node(NodeData::Element(crate::pool::PoolBox::new(
            ElementData::new(qual_name!("a"), vec![]),
        )));

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is not disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );

        let mut mutator = document.mutate();
        mutator.set_attribute(id, qual_name!("disabled"), "");
        drop(mutator);

        let node = document.get_node(id).unwrap();
        assert!(
            !node.element_state().contains(ElementState::DISABLED),
            "form node is not disabled"
        );
        assert!(
            !node.element_state().contains(ElementState::ENABLED),
            "form node is enabled"
        );
    }

    #[test]
    fn mutator_id_attribute_updates_id_map() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id();

        let node_id = {
            let mut mutator = document.mutate();
            let node_id = mutator.create_element(
                qual_name!("div"),
                vec![Attribute::new(qual_name!("id"), "old")],
            );
            mutator.append_children(root_id, &[node_id]);
            node_id
        };
        assert_eq!(document.get_element_by_id("old"), Some(node_id));

        {
            let mut mutator = document.mutate();
            mutator.set_attribute(node_id, qual_name!("id"), "new");
        }
        assert_eq!(document.get_element_by_id("new"), Some(node_id));
        assert_eq!(document.get_element_by_id("old"), None);

        {
            let mut mutator = document.mutate();
            mutator.clear_attribute(node_id, qual_name!("id"));
        }
        assert_eq!(document.get_element_by_id("new"), None);
    }

    #[test]
    fn get_element_by_id_duplicate_ids_first_in_tree_order_wins() {
        let mut document = BaseDocument::new(DocumentConfig::default());
        let root_id = document.root_node().id();

        let (first_id, second_id) = {
            let mut mutator = document.mutate();
            let first_id = mutator.create_element(qual_name!("div"), vec![]);
            let second_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.append_children(root_id, &[first_id, second_id]);
            // Assign the id to the later node first so that insertion order
            // differs from tree order
            mutator.set_attribute(second_id, qual_name!("id"), "dup");
            mutator.set_attribute(first_id, qual_name!("id"), "dup");
            (first_id, second_id)
        };
        assert_eq!(document.get_element_by_id("dup"), Some(first_id));

        {
            let mut mutator = document.mutate();
            mutator.remove_node(first_id);
        }
        assert_eq!(document.get_element_by_id("dup"), Some(second_id));
    }

    #[derive(Default)]
    struct RedrawShell {
        redraw_requests: AtomicUsize,
    }

    impl ShellProvider for RedrawShell {
        fn request_redraw(&self) {
            self.redraw_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn mutator_requests_redraw_only_after_mutation() {
        let shell = Arc::new(RedrawShell::default());
        let mut document = BaseDocument::new(DocumentConfig {
            shell_provider: Some(shell.clone()),
            ..Default::default()
        });
        let root_id = document.root_node().id();

        {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("span"), vec![]);
            mutator.append_children(parent_id, &[child_id]);
            mutator.remove_and_drop_all_children(parent_id);
            mutator.set_attribute(parent_id, qual_name!("id"), "detached");
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 0);

        {
            let mutator = document.mutate();
            assert_eq!(mutator.child_ids(root_id).len(), 0);
        }

        {
            let mut mutator = document.mutate();
            let node_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.append_children(root_id, &[node_id]);
            mutator.set_attribute(node_id, qual_name!("id"), "in-document");
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 1);

        {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("span"), vec![]);
            mutator.append_children(root_id, &[parent_id]);
            mutator.append_children(parent_id, &[child_id]);
            mutator.remove_and_drop_all_children(parent_id);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);

        {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("span"), vec![]);
            let detached_target_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.append_children(root_id, &[parent_id]);
            mutator.append_children(parent_id, &[child_id]);
            assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);
            mutator.append_children(detached_target_id, &[child_id]);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 3);
    }

    #[cfg(feature = "devtools")]
    #[test]
    fn devtools_provider_records_in_document_mutations() {
        use blitz_traits::devtools::{
            AttributeChanged, CharacterDataChanged, DevtoolsProvider, MutationRecord, NodeInserted,
            NodeRemoved,
        };
        use std::sync::Mutex;

        #[derive(Default)]
        struct Recorder(Mutex<Vec<MutationRecord>>);
        impl DevtoolsProvider for Recorder {
            fn mutations(&self, records: &[MutationRecord]) {
                self.0.lock().unwrap().extend_from_slice(records);
            }
        }

        let inserted = |parent_id, node_id, previous_sibling_id| {
            MutationRecord::NodeInserted(NodeInserted {
                parent_id,
                node_id,
                previous_sibling_id,
            })
        };
        let removed =
            |parent_id, node_id| MutationRecord::NodeRemoved(NodeRemoved { parent_id, node_id });
        let attr_changed = |node_id, name: &str| {
            MutationRecord::AttributeChanged(AttributeChanged {
                node_id,
                name: name.into(),
            })
        };
        let text_changed =
            |node_id| MutationRecord::CharacterDataChanged(CharacterDataChanged { node_id });

        let recorder = Arc::new(Recorder::default());
        let mut document = BaseDocument::new(DocumentConfig {
            devtools_provider: Some(recorder.clone()),
            ..Default::default()
        });
        let root_id = document.root_node().id;
        let take = || std::mem::take(&mut *recorder.0.lock().unwrap());

        // Building a detached subtree is not reported; inserting it reports its root only
        let mut mutator = document.mutate();
        let div = mutator.create_element(qual_name!("div"), vec![]);
        let text = mutator.create_text_node("a");
        mutator.append_children(div, &[text]);
        mutator.set_attribute(div, qual_name!("id"), "x");
        mutator.set_node_text(text, "b");
        mutator.flush();
        assert!(take().is_empty());
        // Mutations are delivered when the mutator is flushed
        mutator.append_children(root_id, &[div]);
        assert!(take().is_empty());
        mutator.flush();
        assert_eq!(take(), [inserted(root_id, div, None)]);

        mutator.set_attribute(div, qual_name!("class"), "c");
        mutator.clear_attribute(div, qual_name!("class"));
        mutator.clear_attribute(div, qual_name!("class"));
        mutator.set_node_text(text, "c");
        mutator.set_node_text(text, "c");
        mutator.append_text_to_node(text, "d").unwrap();
        let comment = mutator.create_comment_node("e");
        mutator.set_node_text(comment, "f");
        mutator.append_children(div, &[comment]);
        mutator.set_node_text(comment, "g");
        mutator.set_node_text(comment, "g");
        mutator.flush();
        assert_eq!(
            take(),
            [
                attr_changed(div, "class"),
                attr_changed(div, "class"),
                text_changed(text),
                text_changed(text),
                inserted(div, comment, Some(text)),
                text_changed(comment),
            ]
        );
        assert!(matches!(
            &mutator.doc.get_node(comment).unwrap().data,
            NodeData::Comment { contents } if contents == "g"
        ));

        // Moving a node reports its removal and insertion
        let span = mutator.create_element(qual_name!("span"), vec![]);
        mutator.append_children(root_id, &[span]);
        mutator.append_children(span, &[text]);
        mutator.remove_node(span);
        mutator.append_children(root_id, &[span]);
        mutator.remove_and_drop_all_children(span);
        mutator.remove_and_drop_node(div);
        mutator.flush();
        assert_eq!(
            take(),
            [
                inserted(root_id, span, Some(div)),
                removed(div, text),
                inserted(span, text, None),
                removed(root_id, span),
                inserted(root_id, span, Some(div)),
                removed(span, text),
                removed(root_id, div),
            ]
        );

        // Whitespace-only text is skipped when finding the previous sibling
        let ws = mutator.create_text_node(" ");
        let b = mutator.create_element(qual_name!("b"), vec![]);
        mutator.append_children(span, &[ws, b]);
        mutator.flush();
        assert_eq!(take(), [inserted(span, ws, None), inserted(span, b, None)]);
        drop(mutator);
    }

    #[test]
    fn moving_subtree_out_of_document_clears_in_document_flag() {
        let shell = Arc::new(RedrawShell::default());
        let mut document = BaseDocument::new(DocumentConfig {
            shell_provider: Some(shell.clone()),
            ..Default::default()
        });
        let root_id = document.root_node().id();
        let (child_id, grandchild_id, detached_parent_id) = {
            let mut mutator = document.mutate();
            let in_document_parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let child_id = mutator.create_element(qual_name!("div"), vec![]);
            let grandchild_id = mutator.create_element(qual_name!("span"), vec![]);
            let detached_parent_id = mutator.create_element(qual_name!("section"), vec![]);
            mutator.append_children(root_id, &[in_document_parent_id]);
            mutator.append_children(in_document_parent_id, &[child_id]);
            mutator.append_children(child_id, &[grandchild_id]);
            (child_id, grandchild_id, detached_parent_id)
        };
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 1);
        assert!(document.get_node(child_id).unwrap().flags.is_in_document());
        assert!(
            document
                .get_node(grandchild_id)
                .unwrap()
                .flags
                .is_in_document()
        );

        {
            let mut mutator = document.mutate();
            mutator.append_children(detached_parent_id, &[child_id]);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);
        assert!(!document.get_node(child_id).unwrap().flags.is_in_document());
        assert!(
            !document
                .get_node(grandchild_id)
                .unwrap()
                .flags
                .is_in_document()
        );

        {
            let mut mutator = document.mutate();
            mutator.set_attribute(child_id, qual_name!("id"), "detached");
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 2);

        {
            let mut mutator = document.mutate();
            mutator.append_children(root_id, &[child_id]);
        }
        assert_eq!(shell.redraw_requests.load(Ordering::Relaxed), 3);
        assert!(document.get_node(child_id).unwrap().flags.is_in_document());
        assert!(
            document
                .get_node(grandchild_id)
                .unwrap()
                .flags
                .is_in_document()
        );
    }

    #[test]
    fn style_property_updates_nested_layout() {
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = document.root_node().id();

        let mover_id = {
            let mut mutator = document.mutate();
            let parent_id = mutator.create_element(qual_name!("div"), vec![]);
            let mover_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.set_style_property(parent_id, "position", "relative");
            mutator.set_style_property(parent_id, "width", "800px");
            mutator.set_style_property(parent_id, "height", "600px");
            mutator.set_style_property(mover_id, "position", "absolute");
            mutator.set_style_property(mover_id, "left", "0px");
            mutator.set_style_property(mover_id, "top", "0px");
            mutator.append_children(parent_id, &[mover_id]);
            mutator.append_children(root_id, &[parent_id]);
            mover_id
        };

        document.resolve(0.0);
        assert_eq!(
            document
                .get_node(mover_id)
                .unwrap()
                .final_layout()
                .location
                .x,
            0.0
        );

        {
            let mut mutator = document.mutate();
            mutator.set_style_property(mover_id, "left", "120px");
        }

        document.resolve(0.0);
        assert_eq!(
            document
                .get_node(mover_id)
                .unwrap()
                .final_layout()
                .location
                .x,
            120.0
        );
    }

    /// Hit testing before the first resolve reaches elements that have no computed styles
    /// yet: a pointer already over a window as it opens.
    #[test]
    fn hit_testing_before_the_first_resolve() {
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = document.root_node().id();
        {
            let mut mutator = document.mutate();
            let html = mutator.create_element(qual_name!("html", html), vec![]);
            mutator.append_children(root_id, &[html]);
        }
        // Nothing is laid out yet, so nothing is hit; what matters is that it doesn't panic.
        assert!(document.hit(10.0, 10.0).is_none());
    }

    fn dir_test_tree(css: &str) -> (BaseDocument, NodeId, NodeId, NodeId) {
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ua_stylesheets: vec![UserAgentStylesheet::parse(css)],
            ..Default::default()
        });
        let root_id = document.root_node().id();
        let (outer, x, span) = {
            let mut mutator = document.mutate();
            let outer = mutator.create_element(qual_name!("div"), vec![]);
            let x = mutator.create_element(
                qual_name!("div"),
                vec![Attribute::new(qual_name!("id"), "x")],
            );
            let span = mutator.create_element(qual_name!("span"), vec![]);
            mutator.append_children(outer, &[x, span]);
            mutator.append_children(root_id, &[outer]);
            (outer, x, span)
        };
        document.resolve(0.0);
        (document, outer, x, span)
    }

    fn opacity(document: &BaseDocument, node_id: NodeId) -> f32 {
        document
            .get_node(node_id)
            .unwrap()
            .primary_styles()
            .unwrap()
            .get_effects()
            .opacity
    }

    #[test]
    fn ancestor_dir_and_lang_writes_reach_sibling_selectors() {
        let (mut document, outer, _, span) =
            dir_test_tree("#x:dir(rtl) + span, #x:lang(fr) + span { opacity: 0.5 }");
        assert_eq!(opacity(&document, span), 1.0);

        document
            .mutate()
            .set_attribute(outer, qual_name!("dir"), "rtl");
        document.resolve(0.0);
        assert_eq!(opacity(&document, span), 0.5);

        document.mutate().clear_attribute(outer, qual_name!("dir"));
        document.resolve(0.0);
        assert_eq!(opacity(&document, span), 1.0);

        document
            .mutate()
            .set_attribute(outer, qual_name!("lang"), "fr");
        document.resolve(0.0);
        assert_eq!(opacity(&document, span), 0.5);

        document.mutate().clear_attribute(outer, qual_name!("lang"));
        document.resolve(0.0);
        assert_eq!(opacity(&document, span), 1.0);
    }

    #[test]
    fn own_dir_write_reaches_sibling_dir_selectors() {
        let (mut document, _, x, span) = dir_test_tree("#x:dir(rtl) + span { opacity: 0.5 }");
        assert_eq!(opacity(&document, span), 1.0);

        document.mutate().set_attribute(x, qual_name!("dir"), "rtl");
        document.resolve(0.0);
        assert_eq!(opacity(&document, span), 0.5);
    }

    #[test]
    fn style_property_mutation_copies_blocks_in_rule_tree() {
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = document.root_node().id();

        let div_id = {
            let mut mutator = document.mutate();
            let div_id = mutator.create_element(qual_name!("div"), vec![]);
            mutator.set_style_property(div_id, "width", "100px");
            mutator.set_style_property(div_id, "height", "50px");
            mutator.append_children(root_id, &[div_id]);
            div_id
        };

        let style_block = |document: &BaseDocument| {
            document
                .get_node(div_id)
                .unwrap()
                .element_data()
                .unwrap()
                .style_attribute()
                .cloned()
                .unwrap()
        };

        let before_resolve = style_block(&document);
        {
            let mut mutator = document.mutate();
            mutator.set_style_property(div_id, "height", "60px");
        }
        // Not yet in the rule tree: mutated in place.
        assert!(ServoArc::ptr_eq(&before_resolve, &style_block(&document)));

        document.resolve(0.0);
        let in_rule_tree = style_block(&document);
        assert!(
            in_rule_tree
                .read_with(&document.guard.read())
                .immutable
                .load(Ordering::Relaxed)
        );

        {
            let mut mutator = document.mutate();
            mutator.set_style_property(div_id, "width", "200px");
        }
        let after_set = style_block(&document);
        assert!(!ServoArc::ptr_eq(&in_rule_tree, &after_set));
        {
            let guard = document.guard.read();
            let old = in_rule_tree.read_with(&guard);
            let new = after_set.read_with(&guard);
            assert_eq!(old.declarations().len(), 2);
            assert_eq!(new.declarations().len(), 2);
            assert_ne!(old, new);
        }

        document.resolve(0.0);
        assert_eq!(
            document.get_node(div_id).unwrap().final_layout().size.width,
            200.0
        );

        let in_rule_tree = style_block(&document);
        {
            let mut mutator = document.mutate();
            mutator.remove_style_property(div_id, "height");
        }
        let after_remove = style_block(&document);
        assert!(!ServoArc::ptr_eq(&in_rule_tree, &after_remove));
        {
            let guard = document.guard.read();
            assert_eq!(in_rule_tree.read_with(&guard).declarations().len(), 2);
            assert_eq!(after_remove.read_with(&guard).declarations().len(), 1);
        }
    }

    fn qual_name(name: &str) -> crate::QualName {
        crate::QualName::new(None, crate::ns!(), crate::LocalName::from(name))
    }

    fn attr(name: &str, value: &str) -> Attribute {
        Attribute::new(qual_name(name), value.to_string())
    }

    fn div(m: &mut DocumentMutator<'_>, attrs: &[(&str, &str)]) -> NodeId {
        let attrs = attrs.iter().map(|(k, v)| attr(k, v)).collect::<Vec<_>>();
        m.create_element(qual_name!("div"), attrs)
    }

    /// Tag, layout and resolved style of every element and `::before`/`::after`, in tree order.
    type Snapshot = Vec<(String, [f32; 4], Vec<String>)>;

    fn snapshot(doc: &BaseDocument) -> Snapshot {
        let mut out = Vec::new();
        let mut stack = vec![(doc.root_node().id(), "")];
        while let Some((id, pseudo)) = stack.pop() {
            let node = doc.get_node(id).unwrap();
            if let Some(el) = node.element_data() {
                let l = node.final_layout();
                let rect = [l.location.x, l.location.y, l.size.width, l.size.height];
                let style = crate::resolved_style_property_names()
                    .iter()
                    .map(|name| doc.resolved_style_value(id, name))
                    .collect();
                out.push((format!("{}{pseudo}", el.name.local), rect, style));
            }
            stack.extend(node.after().map(|id| (id, "::after")));
            stack.extend(node.children().iter().rev().map(|id| (id, "")));
            stack.extend(node.before().map(|id| (id, "::before")));
        }
        out
    }

    /// Asserts that the incremental result matches the fresh one, element by element,
    /// naming the first differing property.
    fn assert_matches_fresh(incremental: &Snapshot, fresh: &Snapshot) {
        let boxes = |s: &Snapshot| -> Vec<(String, [f32; 4])> {
            s.iter().map(|(n, r, _)| (n.clone(), *r)).collect()
        };
        assert_eq!(
            boxes(incremental),
            boxes(fresh),
            "incremental layout differs from a fresh layout of the mutated document"
        );
        for (i, ((name, _, inc), (_, _, fre))) in incremental.iter().zip(fresh).enumerate() {
            let differing: Vec<String> = crate::resolved_style_property_names()
                .iter()
                .zip(inc.iter().zip(fre))
                .filter(|(_, (a, b))| a != b)
                .map(|(property, (a, b))| format!("{property}: {a:?} vs {b:?}"))
                .collect();
            assert!(
                differing.is_empty(),
                "resolved style of {name} (element {i}) differs from a fresh layout of the mutated document: {}",
                differing.join(", ")
            );
        }
    }

    /// Checks that a document mutated after its first layout ends up with the same layout
    /// and styles as a document built directly in the mutated state. `build` returns the
    /// node ids that `mutate` operates on; the mutation must change the layout or styles.
    fn check_incremental_restyle(
        css: &str,
        build: impl Fn(&mut DocumentMutator<'_>) -> Vec<NodeId>,
        mutate: impl Fn(&mut DocumentMutator<'_>, &[NodeId]),
    ) {
        check_incremental_restyle_steps(css, build, &[&mutate]);
    }

    /// One mutation batch of [`check_incremental_restyle_steps`].
    type Step<'a> = &'a dyn Fn(&mut DocumentMutator<'_>, &[NodeId]);

    /// Like [`check_incremental_restyle`], but each step is a separate mutation batch
    /// with a layout in between; the fresh document applies all steps before its layout.
    fn check_incremental_restyle_steps(
        css: &str,
        build: impl Fn(&mut DocumentMutator<'_>) -> Vec<NodeId>,
        steps: &[Step<'_>],
    ) {
        let new_document = || {
            let mut doc = BaseDocument::new(DocumentConfig {
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                ..Default::default()
            });
            doc.add_user_agent_stylesheet(crate::UserAgentStylesheet::parse(&format!(
                "html, div {{ display: block }} {css}"
            )));
            let root_id = doc.root_node().id();
            let ids = {
                let mut m = doc.mutate();
                let html = m.create_element(qual_name!("html"), vec![]);
                m.append_children(root_id, &[html]);
                build(&mut m)
            };
            (doc, ids)
        };

        let (mut incremental, ids) = new_document();
        incremental.resolve(0.0);
        let before = snapshot(&incremental);
        for mutate in steps {
            mutate(&mut incremental.mutate(), &ids);
            incremental.resolve(0.0);
        }
        let after = snapshot(&incremental);

        let (mut fresh, ids) = new_document();
        for mutate in steps {
            mutate(&mut fresh.mutate(), &ids);
        }
        fresh.resolve(0.0);
        assert_matches_fresh(&after, &snapshot(&fresh));
        assert_ne!(
            before, after,
            "the mutation should change the layout or styles"
        );
    }

    fn parent_with_children(m: &mut DocumentMutator<'_>, count: usize) -> Vec<NodeId> {
        let root_id = m.doc.root_element().id();
        let parent = div(m, &[]);
        let children: Vec<NodeId> = (0..count).map(|_| div(m, &[])).collect();
        m.append_children(parent, &children);
        m.append_children(root_id, &[parent]);
        std::iter::once(parent).chain(children).collect()
    }

    /// `html > parent.hidden > [child, child]`, ids in that order; `.hidden` is `display: none`.
    fn hidden_parent_with_children(m: &mut DocumentMutator<'_>, count: usize) -> Vec<NodeId> {
        let ids = parent_with_children(m, count);
        m.set_attribute(ids[0], qual_name!("class"), "hidden");
        ids
    }

    const HIDDEN_CSS: &str = "div { height: 10px; width: 100px } .hidden { display: none }";

    fn unhide(m: &mut DocumentMutator<'_>, ids: &[NodeId]) {
        m.clear_attribute(ids[0], qual_name!("class"));
    }

    #[test]
    fn attribute_change_in_hidden_subtree_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} [data-a] ~ div {{ width: 200px; color: red }}"),
            |m| hidden_parent_with_children(m, 3),
            &[
                &|m, ids| m.set_attribute(ids[1], qual_name("data-a"), "1"),
                &unhide,
            ],
        );
    }

    #[test]
    fn class_change_in_hidden_subtree_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} .x {{ width: 200px }} .x div {{ color: red }}"),
            |m| {
                let ids = hidden_parent_with_children(m, 1);
                let grandchild = div(m, &[]);
                m.append_children(ids[1], &[grandchild]);
                ids
            },
            &[
                &|m, ids| m.set_attribute(ids[1], qual_name!("class"), "x"),
                &unhide,
            ],
        );
    }

    #[test]
    fn style_change_in_hidden_subtree_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} .half {{ width: 50% }}"),
            |m| {
                let ids = hidden_parent_with_children(m, 1);
                let grandchild = div(m, &[("class", "half")]);
                m.append_children(ids[1], &[grandchild]);
                ids
            },
            &[
                &|m, ids| m.set_attribute(ids[1], qual_name!("style"), "width: 400px"),
                &unhide,
            ],
        );
    }

    #[test]
    fn insert_in_hidden_subtree_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} div:last-child {{ height: 30px }} div:empty {{ width: 20px }}"),
            |m| hidden_parent_with_children(m, 1),
            &[
                &|m, ids| {
                    let child = div(m, &[]);
                    m.append_children(ids[0], &[child]);
                },
                &unhide,
            ],
        );
    }

    #[test]
    fn remove_in_hidden_subtree_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} div:first-child {{ height: 30px }}"),
            |m| hidden_parent_with_children(m, 2),
            &[
                &|m, ids| {
                    m.remove_and_drop_node(ids[1]);
                },
                &unhide,
            ],
        );
    }

    #[test]
    fn pseudo_rule_matched_in_hidden_subtree_then_unhide() {
        check_incremental_restyle_steps(
            &format!(
                "{HIDDEN_CSS} .x > div::before {{ content: 'a'; display: block; height: 20px }}"
            ),
            |m| {
                let ids = hidden_parent_with_children(m, 1);
                let grandchild = div(m, &[]);
                m.append_children(ids[1], &[grandchild]);
                ids
            },
            &[
                &|m, ids| m.set_attribute(ids[1], qual_name!("class"), "x"),
                &unhide,
            ],
        );
    }

    #[test]
    fn unhide_and_attribute_change_in_one_batch() {
        check_incremental_restyle(
            &format!("{HIDDEN_CSS} [data-a] ~ div {{ width: 200px; color: red }}"),
            |m| hidden_parent_with_children(m, 3),
            |m, ids| {
                unhide(m, ids);
                m.set_attribute(ids[1], qual_name("data-a"), "1");
            },
        );
    }

    #[test]
    fn hide_and_attribute_change_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} [data-a] ~ div {{ width: 200px; color: red }}"),
            |m| parent_with_children(m, 3),
            &[
                &|m, ids| {
                    m.set_attribute(ids[0], qual_name!("class"), "hidden");
                    m.set_attribute(ids[1], qual_name("data-a"), "1");
                },
                &unhide,
            ],
        );
    }

    #[test]
    fn hide_then_mutate_then_unhide() {
        check_incremental_restyle_steps(
            &format!("{HIDDEN_CSS} .x {{ color: red }} div:empty {{ width: 20px }}"),
            |m| parent_with_children(m, 2),
            &[
                &|m, ids| m.set_attribute(ids[0], qual_name!("class"), "hidden"),
                &|m, ids| {
                    m.set_attribute(ids[1], qual_name!("class"), "x");
                    m.remove_and_drop_node(ids[2]);
                },
                &unhide,
            ],
        );
    }

    #[test]
    fn class_change_restyles_descendants() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } .wide div { width: 300px }",
            |m| parent_with_children(m, 1),
            |m, ids| m.set_attribute(ids[0], qual_name!("class"), "wide"),
        );
    }

    #[test]
    fn class_removal_restyles_descendants() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } .wide div { width: 300px }",
            |m| {
                let ids = parent_with_children(m, 1);
                m.set_attribute(ids[0], qual_name!("class"), "wide");
                ids
            },
            |m, ids| m.clear_attribute(ids[0], qual_name!("class")),
        );
    }

    #[test]
    fn id_change_restyles_children() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } #x > div { height: 50px }",
            |m| parent_with_children(m, 2),
            |m, ids| m.set_attribute(ids[0], qual_name!("id"), "x"),
        );
    }

    #[test]
    fn attribute_change_restyles_later_siblings() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } [data-a] ~ div { width: 200px }",
            |m| parent_with_children(m, 3),
            |m, ids| m.set_attribute(ids[1], qual_name("data-a"), "1"),
        );
    }

    #[test]
    fn style_attribute_change_restyles_inheriting_descendants() {
        check_incremental_restyle(
            "div { height: 10px } .em { width: 2em }",
            |m| {
                let root_id = m.doc.root_element().id();
                let parent = div(m, &[]);
                let child = div(m, &[("class", "em")]);
                m.append_children(parent, &[child]);
                m.append_children(root_id, &[parent]);
                vec![parent, child]
            },
            |m, ids| m.set_attribute(ids[0], qual_name!("style"), "font-size: 40px"),
        );
    }

    #[test]
    fn style_width_change_relayouts_descendants() {
        check_incremental_restyle(
            "div { height: 10px } .half { width: 50% }",
            |m| {
                let root_id = m.doc.root_element().id();
                let parent = div(m, &[("style", "width: 400px")]);
                let child = div(m, &[("class", "half")]);
                m.append_children(parent, &[child]);
                m.append_children(root_id, &[parent]);
                vec![parent, child]
            },
            |m, ids| m.set_style_property(ids[0], "width", "200px"),
        );
    }

    #[test]
    fn appending_a_child_restyles_empty_parent() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } div:empty { height: 30px }",
            |m| parent_with_children(m, 0),
            |m, ids| {
                let child = div(m, &[]);
                m.append_children(ids[0], &[child]);
            },
        );
    }

    #[test]
    fn removing_the_last_child_restyles_empty_parent() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } div:empty { height: 30px }",
            |m| parent_with_children(m, 1),
            |m, ids| {
                m.remove_and_drop_node(ids[1]);
            },
        );
    }

    #[test]
    fn inserting_a_child_restyles_nth_child_siblings() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } div:nth-child(2n) { width: 200px }",
            |m| parent_with_children(m, 3),
            |m, ids| {
                let child = div(m, &[]);
                m.insert_nodes_before(ids[1], &[child]);
            },
        );
    }

    #[test]
    fn removing_a_child_restyles_nth_child_siblings() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } div:nth-child(2n) { width: 200px }",
            |m| parent_with_children(m, 3),
            |m, ids| {
                m.remove_and_drop_node(ids[1]);
            },
        );
    }

    #[test]
    fn appending_a_child_restyles_previous_last_child() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } div:last-child { width: 200px }",
            |m| parent_with_children(m, 2),
            |m, ids| {
                let child = div(m, &[]);
                m.append_children(ids[0], &[child]);
            },
        );
    }

    #[test]
    fn removing_the_first_child_restyles_new_first_child() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } div:first-child { width: 200px }",
            |m| parent_with_children(m, 2),
            |m, ids| {
                m.remove_and_drop_node(ids[1]);
            },
        );
    }

    #[test]
    fn inserting_a_sibling_restyles_later_siblings() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } .a ~ div { width: 200px }",
            |m| parent_with_children(m, 2),
            |m, ids| {
                let child = div(m, &[("class", "a")]);
                m.insert_nodes_before(ids[1], &[child]);
            },
        );
    }

    #[test]
    fn style_change_on_inline_element_rebuilds_inline_layout() {
        check_incremental_restyle(
            "div { width: 300px; font-size: 20px }",
            |m| {
                let root_id = m.doc.root_element().id();
                let block = div(m, &[]);
                let before = m.create_text_node("Text is available under the ");
                let link = m.create_element(qual_name!("a"), vec![]);
                let link_text =
                    m.create_text_node("Creative Commons Attribution-ShareAlike 4.0 License");
                let after = m.create_text_node("; additional terms may apply.");
                m.append_children(link, &[link_text]);
                m.append_children(block, &[before, link, after]);
                m.append_children(root_id, &[block]);
                vec![block, link]
            },
            |m, ids| m.set_attribute(ids[1], qual_name!("style"), "font-size: 40px"),
        );
    }

    #[test]
    fn moving_a_child_restyles_both_parents() {
        check_incremental_restyle(
            "div { height: 10px; width: 50px } div:empty { height: 30px } div:last-child { width: 200px }",
            |m| {
                let root_id = m.doc.root_element().id();
                let a = div(m, &[]);
                let b = div(m, &[]);
                let child = div(m, &[]);
                let other = div(m, &[]);
                m.append_children(a, &[child, other]);
                m.append_children(root_id, &[a, b]);
                vec![a, b, child]
            },
            |m, ids| m.append_children(ids[1], &[ids[2]]),
        );
    }

    #[test]
    fn class_change_recolors_descendants() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } .x div { color: red }",
            |m| parent_with_children(m, 1),
            |m, ids| m.set_attribute(ids[0], qual_name!("class"), "x"),
        );
    }

    const PSEUDO_CSS: &str = "div { height: 10px; width: 100px } \
        .x > div::before { content: 'a'; display: block; height: 20px }";

    #[test]
    fn class_change_creates_before_pseudo_on_children() {
        check_incremental_restyle(
            PSEUDO_CSS,
            |m| parent_with_children(m, 1),
            |m, ids| m.set_attribute(ids[0], qual_name!("class"), "x"),
        );
    }

    #[test]
    fn class_removal_removes_before_pseudo_on_children() {
        check_incremental_restyle(
            PSEUDO_CSS,
            |m| {
                let ids = parent_with_children(m, 1);
                m.set_attribute(ids[0], qual_name!("class"), "x");
                ids
            },
            |m, ids| m.clear_attribute(ids[0], qual_name!("class")),
        );
    }

    #[test]
    fn attribute_change_creates_after_pseudo_on_later_siblings() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } \
             [data-a] ~ div::after { content: 'a'; display: block; height: 20px }",
            |m| parent_with_children(m, 2),
            |m, ids| m.set_attribute(ids[1], qual_name("data-a"), ""),
        );
    }

    #[test]
    fn class_change_recolors_before_pseudo_on_children() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } \
             div::before { content: 'a'; display: block; height: 20px } \
             .x > div::before { color: red }",
            |m| parent_with_children(m, 1),
            |m, ids| m.set_attribute(ids[0], qual_name!("class"), "x"),
        );
    }

    #[test]
    fn attribute_change_recolors_after_pseudo_on_later_siblings() {
        check_incremental_restyle(
            "div { height: 10px; width: 100px } \
             div::after { content: 'a'; display: block; height: 20px } \
             [data-a] ~ div::after { color: red }",
            |m| parent_with_children(m, 2),
            |m, ids| m.set_attribute(ids[1], qual_name("data-a"), ""),
        );
    }

    /// Like `check_incremental_restyle`, with the focus on `ids[focus]` before the first layout.
    fn check_incremental_restyle_with_focus(
        css: &str,
        focus: usize,
        build: impl Fn(&mut DocumentMutator<'_>) -> Vec<NodeId>,
        mutate: impl Fn(&mut DocumentMutator<'_>, &[NodeId]),
    ) {
        let new_document = || {
            let mut doc = BaseDocument::new(DocumentConfig {
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                ..Default::default()
            });
            doc.add_user_agent_stylesheet(crate::UserAgentStylesheet::parse(&format!(
                "html, div {{ display: block }} {css}"
            )));
            let root_id = doc.root_node().id();
            let ids = {
                let mut m = doc.mutate();
                let html = m.create_element(qual_name!("html"), vec![]);
                m.append_children(root_id, &[html]);
                build(&mut m)
            };
            doc.set_focus_to(ids[focus]);
            (doc, ids)
        };

        let (mut incremental, ids) = new_document();
        incremental.resolve(0.0);
        let before = snapshot(&incremental);
        mutate(&mut incremental.mutate(), &ids);
        incremental.resolve(0.0);
        let after = snapshot(&incremental);

        let (mut fresh, ids) = new_document();
        mutate(&mut fresh.mutate(), &ids);
        fresh.resolve(0.0);
        assert_matches_fresh(&after, &snapshot(&fresh));
        assert_ne!(
            before, after,
            "the mutation should change the layout or styles"
        );
    }

    /// `html > [container > [sibling, holder > [input]], other]`, ids in that order.
    fn focus_tree(m: &mut DocumentMutator<'_>) -> Vec<NodeId> {
        let root_id = m.doc.root_element().id();
        let container = div(m, &[]);
        let sibling = div(m, &[]);
        let holder = div(m, &[]);
        let input = m.create_element(qual_name!("input"), vec![]);
        let other = div(m, &[]);
        m.append_children(holder, &[input]);
        m.append_children(container, &[sibling, holder]);
        m.append_children(root_id, &[container, other]);
        vec![container, sibling, holder, input, other]
    }

    #[test]
    fn removing_the_focused_subtree_restyles_focus_within_ancestors() {
        check_incremental_restyle_with_focus(
            "div { height: 10px; width: 100px } div:focus-within { height: 30px }",
            3,
            focus_tree,
            |m, ids| {
                m.remove_and_drop_node(ids[0]);
            },
        );
    }

    #[test]
    fn moving_the_focused_element_restyles_focus_within_ancestors() {
        check_incremental_restyle_with_focus(
            "div { height: 10px; width: 100px } div:focus-within { height: 30px }",
            3,
            focus_tree,
            |m, ids| m.append_children(ids[4], &[ids[3]]),
        );
    }

    /// `html > parent > [" ", child, child, " "]`, ids `[parent, child, child]`.
    fn parent_with_text_and_children(m: &mut DocumentMutator<'_>, count: usize) -> Vec<NodeId> {
        let root_id = m.doc.root_element().id();
        let parent = div(m, &[]);
        let leading = m.create_text_node(" ");
        let children: Vec<NodeId> = (0..count).map(|_| div(m, &[])).collect();
        let trailing = m.create_text_node(" ");
        m.append_children(parent, &[leading]);
        m.append_children(parent, &children);
        m.append_children(parent, &[trailing]);
        m.append_children(root_id, &[parent]);
        std::iter::once(parent).chain(children).collect()
    }

    #[test]
    fn inserting_before_the_first_element_child_restyles_it_across_text() {
        check_incremental_restyle(
            "div > div:first-child { width: 200px }",
            |m| parent_with_text_and_children(m, 2),
            |m, ids| {
                let new = div(m, &[]);
                m.insert_nodes_before(ids[1], &[new]);
            },
        );
    }

    #[test]
    fn inserting_after_the_last_element_child_restyles_it_across_text() {
        check_incremental_restyle(
            "div > div:last-child { width: 200px }",
            |m| parent_with_text_and_children(m, 2),
            |m, ids| {
                let new = div(m, &[]);
                m.insert_nodes_after(ids[2], &[new]);
            },
        );
    }

    #[test]
    fn removing_the_first_element_child_restyles_the_next_across_text() {
        check_incremental_restyle(
            "div > div:first-child { width: 200px }",
            |m| parent_with_text_and_children(m, 2),
            |m, ids| m.remove_node(ids[1]),
        );
    }

    #[test]
    fn removing_the_last_element_child_restyles_the_previous_across_text() {
        check_incremental_restyle(
            "div > div:last-child { width: 200px }",
            |m| parent_with_text_and_children(m, 2),
            |m, ids| m.remove_node(ids[2]),
        );
    }

    #[test]
    fn removing_the_first_child_restyles_nth_child_siblings() {
        check_incremental_restyle(
            "div > div:nth-child(2) { width: 200px }",
            |m| parent_with_children(m, 3),
            |m, ids| m.remove_node(ids[1]),
        );
    }

    #[test]
    fn removing_children_front_to_back_restyles_nth_child_siblings() {
        check_incremental_restyle(
            "div > div:nth-child(2) { width: 200px } div + div { color: red }",
            |m| parent_with_text_and_children(m, 4),
            |m, ids| {
                m.remove_node(ids[1]);
                m.remove_node(ids[2]);
            },
        );
    }

    #[test]
    fn inserting_at_the_front_restyles_adjacent_sibling_rules() {
        check_incremental_restyle(
            "div + div { color: red } div > div:nth-child(2) { width: 200px }",
            |m| parent_with_text_and_children(m, 3),
            |m, ids| {
                let new = div(m, &[("class", "a")]);
                m.insert_nodes_before(ids[1], &[new]);
            },
        );
    }
}
