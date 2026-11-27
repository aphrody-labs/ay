//! Stylo's DOM traits, implemented for Blitz's nodes.

use std::ptr::NonNull;

use crate::layout::damage::compute_layout_damage;
use crate::node::Node;
use crate::node::NodeData;
use markup5ever::{LocalName, LocalNameStaticSet, Namespace, NamespaceStaticSet, local_name};
use selectors::bloom::BLOOM_HASH_MASK;
use selectors::{
    OpaqueElement,
    attr::{AttrSelectorOperation, NamespaceConstraint},
    matching::{ElementSelectorFlags, MatchingContext, VisitedHandlingMode},
    sink::Push,
};
use style::CaseSensitivityExt;
use style::animation::AnimationSetKey;
use style::applicable_declarations::ApplicableDeclarationBlock;
use style::bloom::each_relevant_element_hash;
use style::data::{ElementDataMut, ElementDataRef};
use style::properties::ComputedValues;
use style::selector_parser::PseudoElement;
use style::selector_parser::RestyleDamage;
use style::stylesheets::scope_rule::ImplicitScopeRoot;
use style::values::AtomString;
use style::{
    context::{QuirksMode, SharedStyleContext},
    dom::{LayoutIterator, NodeInfo, OpaqueNode, TDocument, TElement, TNode, TShadowRoot},
    properties::PropertyDeclarationBlock,
    selector_parser::{NonTSPseudoClass, SelectorImpl},
    servo_arc::{Arc, ArcBorrow},
    shared_lock::{Locked, SharedRwLock},
    values::{AtomIdent, GenericAtomIdent},
};
use style_dom::ElementState;

/// A handle to a node that Servo's style traits are implemented against
///
/// Since BlitzNodes are not persistent (IE we don't keep the pointers around between frames), we choose to just implement
/// the tree structure in the nodes themselves, and temporarily give out pointers during the layout phase.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlitzNode<'a>(pub(crate) &'a Node);

impl std::ops::Deref for BlitzNode<'_> {
    type Target = Node;
    fn deref(&self) -> &Node {
        self.0
    }
}

impl std::fmt::Debug for BlitzNode<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'a> TDocument for BlitzNode<'a> {
    type ConcreteNode = BlitzNode<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn is_html_document(&self) -> bool {
        true
    }

    fn quirks_mode(&self) -> QuirksMode {
        QuirksMode::NoQuirks
    }

    fn shared_lock(&self) -> &SharedRwLock {
        self.guard()
    }
}

impl NodeInfo for BlitzNode<'_> {
    fn is_element(&self) -> bool {
        Node::is_element(self)
    }

    fn is_text_node(&self) -> bool {
        Node::is_text_node(self)
    }
}

impl<'a> TShadowRoot for BlitzNode<'a> {
    type ConcreteNode = BlitzNode<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn host(&self) -> <Self::ConcreteNode as TNode>::ConcreteElement {
        let host_id = self
            .shadow_root_data()
            .expect("host() called on a non-shadow-root node")
            .host;
        BlitzNode(self.0.with(host_id))
    }

    fn style_data<'b>(&self) -> Option<&'b style::stylist::CascadeData>
    where
        Self: 'b,
    {
        // Self: 'b bounds 'a: 'b, so handing out the node's cascade
        // data at 'b is sound (it lives in the slab entry itself).
        let node: &'b Node = self.0;
        node.shadow_root_data().map(|d| &*d.author_styles.data)
    }
}

// components/styleaapper.rs:
impl<'a> TNode for BlitzNode<'a> {
    type ConcreteElement = BlitzNode<'a>;
    type ConcreteDocument = BlitzNode<'a>;
    type ConcreteShadowRoot = BlitzNode<'a>;

    fn parent_node(&self) -> Option<Self> {
        self.0.parent_node().map(BlitzNode)
    }

    fn first_child(&self) -> Option<Self> {
        self.children().first().map(|id| BlitzNode(self.0.with(id)))
    }

    fn last_child(&self) -> Option<Self> {
        self.children().last().map(|id| BlitzNode(self.0.with(id)))
    }

    fn prev_sibling(&self) -> Option<Self> {
        self.0.backward(1).map(BlitzNode)
    }

    fn next_sibling(&self) -> Option<Self> {
        self.0.forward(1).map(BlitzNode)
    }

    fn owner_doc(&self) -> Self::ConcreteDocument {
        BlitzNode(self.0.owner_doc())
    }

    fn is_in_document(&self) -> bool {
        true
    }

    // Flat-tree parent: assigned nodes hang off their slot, shadow
    // top-levels off the host (the shadow-root node is transparent).
    fn traversal_parent(&self) -> Option<Self::ConcreteElement> {
        if let Some(slot_id) = self.0.assigned_slot() {
            return Some(BlitzNode(self.0.with(slot_id)));
        }
        let parent = self.parent_node()?;
        match &parent.data {
            NodeData::ShadowRoot(data) => Some(BlitzNode(parent.0.with(data.host))),
            _ => parent.as_element(),
        }
    }

    fn opaque(&self) -> OpaqueNode {
        Node::opaque(self)
    }

    fn debug_id(self) -> usize {
        self.0.id().as_u64() as usize
    }

    fn as_element(&self) -> Option<Self::ConcreteElement> {
        match self.data {
            NodeData::Element { .. } => Some(*self),
            _ => None,
        }
    }

    fn as_document(&self) -> Option<Self::ConcreteDocument> {
        match self.data {
            NodeData::Document(_) => Some(*self),
            _ => None,
        }
    }

    fn as_shadow_root(&self) -> Option<Self::ConcreteShadowRoot> {
        match self.data {
            NodeData::ShadowRoot { .. } => Some(*self),
            _ => None,
        }
    }
}

impl selectors::Element for BlitzNode<'_> {
    type Impl = SelectorImpl;

    fn opaque(&self) -> selectors::OpaqueElement {
        // This correctly uses a unique id for the OpaqueElement (unlike using a pointer to the "slot")
        // However, it makes it impossible for us to "rehydrate" the OpaqueElement back into an actual Element
        // which is required to implement the `implicit_scope_for_sheet_in_shadow_root` method below
        //
        // We should see if selectors will accept a PR that allows us to use 128bits for the OpaqueElement. Or
        // find some other solution that will enable "rehydration". This is required to enable and use the
        // Shadow DOM functionality in Stylo.
        let non_null =
            NonNull::new((self.0.id().as_u64() as usize).wrapping_add(1) as *mut ()).unwrap();
        OpaqueElement::from_ptr(non_null)
    }

    // Selector matching stays inside the current tree: shadow-tree
    // top-levels have a null parentElement (traversal_parent crosses).
    fn parent_element(&self) -> Option<Self> {
        self.parent_node().and_then(|node| node.as_element())
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        self.parent_id()
            .is_some_and(|id| self.0.with(id).is_shadow_root())
    }

    // ::slotted() matching walks this edge (selectors crate default
    // is None, which disables slotted rules entirely).
    fn assigned_slot(&self) -> Option<Self> {
        // Field access (no parens) reads Node's stored assignment,
        // dodging the method-name collision with this override.
        self.0.assigned_slot().map(|id| BlitzNode(self.0.with(id)))
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        TElement::containing_shadow(self).map(|root| TShadowRoot::host(&root))
    }

    fn is_pseudo_element(&self) -> bool {
        matches!(self.data, NodeData::AnonymousBlock(_))
    }

    // These methods are implemented naively since we only threaded real nodes and not fake nodes
    // we should try and use `find` instead of this foward/backward stuff since its ugly and slow
    // Locate self in the parent's child list once, then scan from there: stepping with
    // forward(n)/backward(n) repeated that lookup for every skipped text node.
    fn prev_sibling_element(&self) -> Option<Self> {
        let parent = self.0.with(self.0.parent_id()?);
        let index = self.0.child_index().unwrap_or(0);
        parent
            .children()
            .iter()
            .take(index)
            .rev()
            .map(|id| self.0.with(id))
            .find(|node| node.is_element())
            .map(BlitzNode)
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let parent = self.0.with(self.0.parent_id()?);
        let index = self.0.child_index().unwrap_or(0);
        parent
            .children()
            .iter()
            .skip(index + 1)
            .map(|id| self.0.with(id))
            .find(|node| node.is_element())
            .map(BlitzNode)
    }

    fn first_element_child(&self) -> Option<Self> {
        self.0.first_element_child().map(BlitzNode)
    }

    fn is_html_element_in_html_document(&self) -> bool {
        true // self.has_namespace(ns!(html))
    }

    fn has_local_name(&self, local_name: &LocalName) -> bool {
        self.data.is_element_with_tag_name(local_name)
    }

    fn has_namespace(&self, ns: &Namespace) -> bool {
        self.element_data().expect("Not an element").ns() == *ns
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.local_name() == other.local_name() && self.namespace() == other.namespace()
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&GenericAtomIdent<NamespaceStaticSet>>,
        local_name: &GenericAtomIdent<LocalNameStaticSet>,
        operation: &AttrSelectorOperation<&AtomString>,
    ) -> bool {
        let Some(attrs) = self.data.attrs() else {
            return false;
        };
        attrs.iter().any(|attr| {
            attr.name.local == local_name.0
                && match ns {
                    NamespaceConstraint::Any => true,
                    NamespaceConstraint::Specific(ns) => attr.ns() == ns.0,
                }
                && operation.eval_str(&attr.value)
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        pseudo_class: &<Self::Impl as selectors::SelectorImpl>::NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        match *pseudo_class {
            NonTSPseudoClass::Active => self.element_state().contains(ElementState::ACTIVE),
            // :dir() matches HTML directionality (not the
            // CSS direction property): unknown idents never match.
            NonTSPseudoClass::Dir(ref dir) => {
                let rtl = self.html_directionality_is_rtl();
                match &**dir {
                    "rtl" => rtl,
                    "ltr" => !rtl,
                    _ => false,
                }
            }
            NonTSPseudoClass::AnyLink => self
                .element_state()
                .intersects(ElementState::VISITED_OR_UNVISITED),
            NonTSPseudoClass::Checked => {
                // Two-tier: layout-tracked state first (covers the
                // .checked IDL property mutation path that real
                // pages use), fall back to the content attribute
                // (covers detached/Fragment elements where layout
                // never ran: the WPT
                // ParentNode-querySelector-All "matching checked
                // radio buttons and checkboxes" pattern).
                //
                // Per HTML §dom-checked-pseudo:
                //   - input[type=checkbox|radio] is :checked when
                //     the *checkedness* is true. Checkedness starts
                //     from the `checked` content attribute and is
                //     then mutated by .checked = ... / user input.
                //   - option is :checked when the `selected` IDL
                //     state is true.
                //
                // SpecialElementData::CheckboxInput tracks live
                // checkedness once the element enters layout, so
                // it's authoritative for rendered inputs. Detached
                // / Fragment-rooted elements never get that data
                // populated, so we fall back to the content attr.
                let elem = self.data.downcast_element();
                if let Some(elem) = elem {
                    if elem.name.local == local_name!("input") {
                        let type_attr = elem.attr(local_name!("type")).unwrap_or("text");
                        let is_checkable = type_attr.eq_ignore_ascii_case("checkbox")
                            || type_attr.eq_ignore_ascii_case("radio");
                        if !is_checkable {
                            false
                        } else if let Some(state) = elem.checkbox_input_checked() {
                            // Live layout-tracked state. Real pages.
                            state
                        } else {
                            // Detached / unrendered. WPT pattern.
                            elem.attr(local_name!("checked")).is_some()
                        }
                    } else if elem.name.local == local_name!("option") {
                        elem.attr(local_name!("selected")).is_some()
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            NonTSPseudoClass::Valid => false,
            NonTSPseudoClass::Invalid => false,
            NonTSPseudoClass::Defined => false,
            NonTSPseudoClass::Disabled => self.element_state().contains(ElementState::DISABLED),
            NonTSPseudoClass::Enabled => self.element_state().contains(ElementState::ENABLED),
            NonTSPseudoClass::Focus => self.element_state().contains(ElementState::FOCUS),
            NonTSPseudoClass::FocusWithin => {
                self.element_state().contains(ElementState::FOCUS_WITHIN)
            }
            NonTSPseudoClass::FocusVisible => false,
            NonTSPseudoClass::Fullscreen => false,
            NonTSPseudoClass::Hover => self.element_state().contains(ElementState::HOVER),
            NonTSPseudoClass::Indeterminate => false,
            NonTSPseudoClass::Lang(ref lang_arg) => {
                // Walk self + ancestors looking for the nearest `lang`
                // attribute. BCP-47 prefix-match (case-insensitive).
                let arg = lang_arg.as_ref();
                if arg.is_empty() {
                    false
                } else {
                    let mut cur = Some(*self);
                    let effective_lang = loop {
                        let Some(node) = cur else { break None };
                        if let Some(value) = node.0.data.attr(local_name!("lang")) {
                            break Some(value);
                        }
                        cur = node.parent_node();
                    };
                    match effective_lang {
                        None => false,
                        Some(lang) => {
                            let lang_b = lang.as_bytes();
                            let arg_b = arg.as_bytes();
                            lang_b.eq_ignore_ascii_case(arg_b)
                                || (lang_b.len() > arg_b.len()
                                    && lang_b[arg_b.len()] == b'-'
                                    && lang_b[..arg_b.len()].eq_ignore_ascii_case(arg_b))
                        }
                    }
                }
            }
            NonTSPseudoClass::CustomState(_) => false,
            NonTSPseudoClass::Link => self.element_state().contains(ElementState::UNVISITED),
            NonTSPseudoClass::PlaceholderShown => false,
            NonTSPseudoClass::ReadWrite => false,
            NonTSPseudoClass::ReadOnly => false,
            NonTSPseudoClass::ServoNonZeroBorder => false,
            NonTSPseudoClass::Target => false,
            NonTSPseudoClass::Visited => false,
            NonTSPseudoClass::Autofill => false,
            NonTSPseudoClass::Default => false,

            NonTSPseudoClass::InRange => false,
            NonTSPseudoClass::Modal => false,
            NonTSPseudoClass::MozTopmostModal => false,
            NonTSPseudoClass::MozInert => self.element_state().contains(ElementState::INERT),
            NonTSPseudoClass::Open => self.element_state().contains(ElementState::OPEN),
            NonTSPseudoClass::Optional => false,
            NonTSPseudoClass::OutOfRange => false,
            NonTSPseudoClass::PopoverOpen => false,
            NonTSPseudoClass::Required => false,
            NonTSPseudoClass::UserInvalid => false,
            NonTSPseudoClass::UserValid => false,
            NonTSPseudoClass::MozMeterOptimum => false,
            NonTSPseudoClass::MozMeterSubOptimum => false,
            NonTSPseudoClass::MozMeterSubSubOptimum => false,
        }
    }

    fn match_pseudo_element(
        &self,
        pe: &PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        let pseudo = match self.try_stylo_element_data().and_then(|s| s.get()) {
            Some(el) => el
                .styles
                .get_primary()
                .and_then(|s| s.pseudo())
                .or(match &self.data {
                    NodeData::AnonymousBlock(_) => Some(PseudoElement::ServoAnonymousBox),
                    _ => None,
                }),
            None => None,
        };

        pseudo.is_some_and(|psuedo| psuedo == *pe)
    }

    fn apply_selector_flags(&self, flags: ElementSelectorFlags) {
        // Handle flags that apply to the element.
        let self_flags = flags.for_self();
        if !self_flags.is_empty() {
            self.selector_flags()
                .set(self.selector_flags().get() | self_flags);
        }

        // Handle flags that apply to the parent.
        let parent_flags = flags.for_parent();
        if !parent_flags.is_empty() {
            // Shadow roots hold no selector flags, and nothing here reads parent
            // flags: mutations under a root restyle through the host's subtree hint.
            if let Some(parent) = self.parent_node().filter(|p| !p.is_shadow_root()) {
                parent
                    .selector_flags()
                    .set(parent.selector_flags().get() | parent_flags);
            }
        }
    }

    fn is_link(&self) -> bool {
        self.data.is_element_with_tag_name(&local_name!("a"))
    }

    // The selectors crate asserts assigned_slot() targets satisfy
    // this; ::slotted() matching debug-panics without it.
    fn is_html_slot_element(&self) -> bool {
        self.is_slot_element()
    }

    fn has_id(
        &self,
        id: &<Self::Impl as selectors::SelectorImpl>::Identifier,
        case_sensitivity: selectors::attr::CaseSensitivity,
    ) -> bool {
        self.element_data()
            .and_then(|data| data.id.as_ref())
            .map(|id_attr| case_sensitivity.eq_atom(id_attr, id))
            .unwrap_or(false)
    }

    fn has_class(
        &self,
        search_name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
        case_sensitivity: selectors::attr::CaseSensitivity,
    ) -> bool {
        self.element_data().is_some_and(|data| {
            data.classes
                .iter()
                .any(|class| case_sensitivity.eq_atom(class, search_name))
        })
    }

    fn imported_part(
        &self,
        _name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
    ) -> Option<<Self::Impl as selectors::SelectorImpl>::Identifier> {
        None
    }

    fn is_part(&self, _name: &<Self::Impl as selectors::SelectorImpl>::Identifier) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        // Per CSS Selectors Level 4 + WPT (matching Chrome/Firefox):
        // comments do not count as content for `:empty`.
        self.dom_children()
            .all(|c| matches!(c.data, NodeData::Comment { .. }))
    }

    fn is_root(&self) -> bool {
        // Per CSS Selectors Level 4 `:root` matches the root element;
        // the element whose parent is a Document node. Detached
        // elements and children of detached subtrees must not match.
        match self.parent_node() {
            Some(parent) => matches!(parent.data, NodeData::Document(_)),
            None => false,
        }
    }

    fn has_custom_state(
        &self,
        _name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
    ) -> bool {
        false
    }

    fn add_element_unique_hashes(&self, filter: &mut selectors::bloom::BloomFilter) -> bool {
        each_relevant_element_hash(*self, |hash| filter.insert_hash(hash & BLOOM_HASH_MASK));
        true
    }
}

impl<'a> TElement for BlitzNode<'a> {
    type ConcreteNode = BlitzNode<'a>;

    type TraversalChildrenIterator = Traverser<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn implicit_scope_for_sheet_in_shadow_root(
        _opaque_host: OpaqueElement,
        _sheet_index: usize,
    ) -> Option<ImplicitScopeRoot> {
        // Unimplementable while OpaqueElement is a bare NodeId (no slab
        // to rehydrate through): see the comment on `opaque()` above.

        // None = no implicit @scope root; only misdirects @scope rules
        // inside shadow sheets, vastly better than a todo!() panic.
        None
    }

    // Flat tree: the Traverser reads flat_children(), so hosts yield
    // shadow children, slots their assigned nodes, others DOM children.
    fn traversal_children(&self) -> style::dom::LayoutIterator<Self::TraversalChildrenIterator> {
        LayoutIterator(Traverser {
            // dom: self.tree(),
            parent: *self,
            child_index: 0,
        })
    }

    fn is_html_element(&self) -> bool {
        self.is_element()
    }

    // not implemented.....
    fn is_mathml_element(&self) -> bool {
        false
    }

    // need to check the namespace
    fn is_svg_element(&self) -> bool {
        false
    }

    // Style inheritance follows the flat tree (parent_element stops
    // at the shadow root by design; inheritance must cross to host).
    fn inheritance_parent(&self) -> Option<Self> {
        TElement::traversal_parent(self)
    }

    fn style_attribute(&self) -> Option<ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        self.element_data()
            .expect("Not an element")
            .style_attribute()
            .as_ref()
            .map(|f| f.borrow_arc())
    }

    fn state(&self) -> ElementState {
        self.element_state().get()
    }

    fn has_part_attr(&self) -> bool {
        false
    }

    fn exports_any_part(&self) -> bool {
        false
    }

    fn id(&self) -> Option<&style::Atom> {
        self.element_data().and_then(|data| data.id.as_ref())
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&style::values::AtomIdent),
    {
        if let Some(data) = self.element_data() {
            for class in &data.classes {
                callback(AtomIdent::cast(class));
            }
        }
    }

    fn each_attr_name<F>(&self, mut callback: F)
    where
        F: FnMut(&style::LocalName),
    {
        if let Some(attrs) = self.data.attrs() {
            for attr in attrs.iter() {
                callback(GenericAtomIdent::cast(&attr.name.local));
            }
        }
    }

    fn has_dirty_descendants(&self) -> bool {
        Node::has_dirty_descendants(self)
    }

    fn has_snapshot(&self) -> bool {
        Node::has_snapshot(self)
    }

    fn handled_snapshot(&self) -> bool {
        self.snapshot_handled()
    }

    unsafe fn set_handled_snapshot(&self) {
        self.set_snapshot_handled(true);
    }

    // Stylo calls this on elements it is already traversing (or has already
    // reached via invalidation), so the ancestor chain must not be re-marked:
    // ancestors visited earlier in the preorder traversal have had their bits
    // cleared, and re-flagging them leaves stale `dirty_descendants` bits that
    // break the early-out invariant of `Node::mark_ancestors_dirty` (a set bit
    // implies all ancestors are set).
    unsafe fn set_dirty_descendants(&self) {
        Node::set_dirty_descendants(self);
    }

    unsafe fn unset_dirty_descendants(&self) {
        Node::unset_dirty_descendants(self);
    }

    fn store_children_to_process(&self, _n: isize) {
        unimplemented!()
    }

    fn did_process_child(&self) -> isize {
        unimplemented!()
    }

    unsafe fn ensure_data(&self) -> ElementDataMut<'_> {
        // SAFETY: stylo traversal has exclusive access to nodes
        unsafe { self.stylo_element_data().ensure_init() }
    }

    unsafe fn clear_data(&self) {
        // SAFETY: stylo traversal has exclusive access to nodes
        unsafe { self.stylo_element_data().clear() }
    }

    fn has_data(&self) -> bool {
        self.try_stylo_element_data().is_some_and(|s| s.has_data())
    }

    fn borrow_data(&self) -> Option<ElementDataRef<'_>> {
        self.try_stylo_element_data().and_then(|s| s.get())
    }

    fn mutate_data(&self) -> Option<ElementDataMut<'_>> {
        unsafe { self.stylo_element_data().unsafe_stylo_only_mut() }
    }

    fn skip_item_display_fixup(&self) -> bool {
        false
    }

    fn may_have_animations(&self) -> bool {
        self.element_data()
            .is_some_and(|data| data.may_have_animations())
    }

    fn set_may_have_animations(&self) {
        if let Some(data) = self.element_data() {
            data.set_may_have_animations(true);
        }
    }

    fn has_animations(&self, context: &SharedStyleContext) -> bool {
        if !self.may_have_animations() {
            return false;
        }
        let key = AnimationSetKey::new_for_non_pseudo(TNode::opaque(&TElement::as_node(self)));
        context
            .animations
            .has_active_animations_or_transitions(&key)
    }

    fn has_css_animations(
        &self,
        context: &SharedStyleContext,
        pseudo_element: Option<PseudoElement>,
    ) -> bool {
        if !self.may_have_animations() {
            return false;
        }
        let key = AnimationSetKey::new(TNode::opaque(&TElement::as_node(self)), pseudo_element);
        context.animations.has_active_animations(&key)
    }

    fn has_css_transitions(
        &self,
        context: &SharedStyleContext,
        pseudo_element: Option<PseudoElement>,
    ) -> bool {
        if !self.may_have_animations() {
            return false;
        }
        let key = AnimationSetKey::new(TNode::opaque(&TElement::as_node(self)), pseudo_element);
        context.animations.has_active_transitions(&key)
    }

    fn animation_rule(
        &self,
        context: &SharedStyleContext,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        let opaque = TNode::opaque(&TElement::as_node(self));
        context.animations.get_animation_declarations(
            &AnimationSetKey::new_for_non_pseudo(opaque),
            context.current_time_for_animations,
            self.guard(),
        )
    }

    fn transition_rule(
        &self,
        context: &SharedStyleContext,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        let opaque = TNode::opaque(&TElement::as_node(self));
        context.animations.get_transition_declarations(
            &AnimationSetKey::new_for_non_pseudo(opaque),
            context.current_time_for_animations,
            self.guard(),
        )
    }

    fn shadow_root(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        self.attached_shadow_root_id()
            .map(|id| BlitzNode(self.0.with(id)))
    }

    // Walking Node::parent terminates: a shadow subtree's chain hits
    // its shadow-root node before ever reaching the host's ancestors.
    fn containing_shadow(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        let mut current = self.parent_node();
        while let Some(node) = current {
            if node.is_shadow_root() {
                return Some(node);
            }
            current = node.parent_node();
        }
        None
    }

    fn get_attr(&self, attr: &style::LocalName, ns: &style::Namespace) -> Option<String> {
        // TODO: case-insensitive matching for HTML-ns attrs
        self.attrs()?
            .iter()
            .find(|a| a.name.local == attr.0 && *a.namespace() == ns.0)
            .map(|a| a.value.to_string())
    }

    fn lang_attr(&self) -> Option<style::selector_parser::AttrValue> {
        None
    }

    fn match_element_lang(
        &self,
        _override_lang: Option<Option<style::selector_parser::AttrValue>>,
        _value: &style::selector_parser::Lang,
    ) -> bool {
        false
    }

    fn is_html_document_body_element(&self) -> bool {
        // Check node is a <body> element
        let is_body_element = self.data.is_element_with_tag_name(&local_name!("body"));

        // If it isn't then return early
        if !is_body_element {
            return false;
        }

        // If it is then check if it is a child of the root (<html>) element
        let root_element = self.0.owner_doc().first_element_child().unwrap();
        root_element.children().contains(self.0.id())
    }

    fn synthesize_presentational_hints_for_legacy_attributes<'h, V>(
        &self,
        _visited_handling: VisitedHandlingMode,
        hints: &mut V,
    ) where
        V: Push<ApplicableDeclarationBlock<'h>>,
        Self: 'h,
    {
        super::presentational_hints::synthesize(self.0, hints);
    }

    fn local_name(&self) -> &LocalName {
        &self.element_data().expect("Not an element").name.local
    }

    fn namespace(&self) -> &Namespace {
        self.element_data().expect("Not an element").namespace()
    }

    fn query_container_size(
        &self,
        _display: &style::values::specified::Display,
    ) -> euclid::default::Size2D<Option<app_units::Au>> {
        // Content-box size from the last layout pass (cq units, size
        // container queries). Blitz doesn't interleave style and
        // layout, so the first cascade sees a zeroed layout; callers
        // converge on the next restyle once the container is laid out.
        let l = self.final_layout();
        let w = l.size.width - l.padding.left - l.padding.right - l.border.left - l.border.right;
        let h = l.size.height - l.padding.top - l.padding.bottom - l.border.top - l.border.bottom;
        euclid::default::Size2D::new(
            Some(app_units::Au::from_f32_px(w.max(0.0))),
            Some(app_units::Au::from_f32_px(h.max(0.0))),
        )
    }

    // No CustomStateSet here, matching `has_custom_state` returning false.
    // The relative-selector collector calls this, so it cannot stay todo!().
    fn each_custom_state<F>(&self, _callback: F)
    where
        F: FnMut(&AtomIdent),
    {
    }

    fn has_selector_flags(&self, flags: ElementSelectorFlags) -> bool {
        self.selector_flags().get().contains(flags)
    }

    fn relative_selector_search_direction(&self) -> ElementSelectorFlags {
        let flags = self.selector_flags().get();
        if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING)
        {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING
        } else if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR)
        {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR
        } else if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING) {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING
        } else {
            ElementSelectorFlags::empty()
        }
    }

    fn compute_layout_damage(old: &ComputedValues, new: &ComputedValues) -> RestyleDamage {
        compute_layout_damage(old, new)
        // ALL_DAMAGE
    }

    // fn update_animations(
    //     &self,
    //     before_change_style: Option<Arc<ComputedValues>>,
    //     tasks: style::context::UpdateAnimationsTasks,
    // ) {
    //     todo!()
    // }

    // fn process_post_animation(&self, tasks: style::context::PostAnimationTasks) {
    //     todo!()
    // }

    // fn needs_transitions_update(
    //     &self,
    //     before_change_style: &ComputedValues,
    //     after_change_style: &ComputedValues,
    // ) -> bool {
    //     todo!()
    // }
}

pub(crate) struct Traverser<'a> {
    // dom: &'a Slab<Node>,
    parent: BlitzNode<'a>,
    child_index: usize,
}

impl<'a> Iterator for Traverser<'a> {
    type Item = BlitzNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let node_id = self.parent.0.flat_children().get(self.child_index)?;
        let node = self.parent.0.with(node_id);

        self.child_index += 1;

        Some(BlitzNode(node))
    }
}

impl std::hash::Hash for BlitzNode<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.id().as_u64())
    }
}
