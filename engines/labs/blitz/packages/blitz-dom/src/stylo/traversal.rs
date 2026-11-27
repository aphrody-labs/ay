//! Running Stylo's style traversal over a document.

use blitz_traits::node_id::NodeId;
use markup5ever::LocalNameStaticSet;
use selectors::matching::ElementSelectorFlags;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use style::values::{AtomIdent, GenericAtomIdent};
use style_dom::ElementState;

use crate::StyleThreading;
use crate::layout::damage::CONSTRUCT_BOX;
use crate::node::{Node, NodeData};
use crate::stylo_to_parley::has_inline_background;
use style::animation::AnimationState;
use style::data::ElementDataFlags;
use style::global_style_data::STYLE_THREAD_POOL;
use style::invalidation::element::element_wrapper::{ElementSnapshot, Snapshots};
use style::invalidation::element::invalidator::SiblingTraversalMap;
use style::invalidation::element::relative_selector::{
    DomMutationOperation, RelativeSelectorInvalidator,
};
use style::invalidation::element::restyle_hints::RestyleHint;
use style::properties::ComputedValues;
use style::selector_parser::RestyleDamage;
use style::{
    Atom,
    context::{
        RegisteredSpeculativePainter, RegisteredSpeculativePainters, SharedStyleContext,
        StyleContext, ThreadLocalStyleContext,
    },
    dom::{TElement, TNode},
    global_style_data::GLOBAL_STYLE_DATA,
    servo_arc::Arc,
    shared_lock::StylesheetGuards,
    stylist::RuleInclusion,
    thread_state::ThreadState,
    traversal::{DomTraversal, recalc_style_at, resolve_style},
    traversal_flags::TraversalFlags,
};

use super::dom::BlitzNode;

/// Serialises `Parallel` style traversals across documents. Stylo's per-worker
/// caches (`StyleSharingCache`, `StyleBloom`) live in native thread-locals that
/// a traversal borrows mutably for its whole duration, so two traversals in
/// flight on the shared pool at once panic with `already mutably borrowed`
/// (<https://github.com/DioxusLabs/blitz/issues/430>).
static PARALLEL_TRAVERSAL_LOCK: Mutex<()> = Mutex::new(());

impl crate::document::BaseDocument {
    pub fn resolve_stylist(&mut self, now: f64) {
        style::thread_state::enter(ThreadState::LAYOUT);

        // Rebuild scoped CascadeData for shadow roots whose <style>
        // set changed; invalidation was done wholesale at mutation.
        let dirty_roots = std::mem::take(&mut self.dirty_shadow_style_roots);
        for shadow_root_id in dirty_roots {
            let Some(node) = self.nodes.get_mut(shadow_root_id) else {
                continue;
            };
            let NodeData::ShadowRoot(ref mut data) = node.data else {
                continue;
            };
            // Steal the styles so stylist + styles can borrow disjointly.
            let mut styles = std::mem::replace(
                &mut data.author_styles,
                style::author_styles::AuthorStyles::new(),
            );
            let _ = styles.flush(&mut self.stylist, &self.guard.read());
            if let NodeData::ShadowRoot(ref mut data) = self.nodes[shadow_root_id].data {
                data.author_styles = styles;
            }
        }

        let guard = &self.guard;
        let guards = StylesheetGuards {
            author: &guard.read(),
            ua_or_user: &guard.read(),
        };

        let root = BlitzNode(self.nodes[self.root_node_id].first_element_child().unwrap());

        self.stylist
            .flush(&guards)
            .process_style(root, Some(&self.snapshots));

        // Mark actively animating nodes as dirty
        let mut sets = self.animations.sets.write();
        for (key, set) in sets.iter_mut() {
            let node_id = self.nodes.id_from_opaque(key.node.id());

            // Drop animations belonging to nodes that are no longer in the
            // document. A removed element is never restyled, so it would never
            // get a chance to cancel its own animations; an infinite animation
            // would then keep `has_active_animations` set forever and force a
            // redraw every frame. Emptying the set here lets the `retain` below
            // discard it so the flag can clear on this same pass.
            let in_document = node_id
                .and_then(|id| self.nodes.get(id))
                .is_some_and(|node| node.flags.is_in_document());
            let Some(node_id) = node_id.filter(|_| in_document) else {
                set.animations.clear();
                set.transitions.clear();
                continue;
            };

            // Only a set with something left to tick can change what it
            // contributes; a finished fill or a paused animation cannot.

            // The hint also walks the ancestor chain, so an idle set costs a
            // recascade of itself plus that walk, on every single resolve.
            if set.needs_animation_ticks() {
                self.nodes[node_id].set_restyle_hint(RestyleHint::RESTYLE_SELF);
            }

            for animation in set.animations.iter_mut() {
                if animation.state == AnimationState::Pending && animation.started_at <= now {
                    animation.state = AnimationState::Running;
                }
                animation.iterate_if_necessary(now);

                if animation.state == AnimationState::Running && animation.has_ended(now) {
                    animation.state = AnimationState::Finished;
                }
            }

            for transition in set.transitions.iter_mut() {
                if transition.state == AnimationState::Pending && transition.start_time <= now {
                    transition.state = AnimationState::Running;
                }
                if transition.state == AnimationState::Running && transition.has_ended(now) {
                    transition.state = AnimationState::Finished;
                }
            }
        }
        drop(sets);

        self.invalidate_relative_selectors();

        // Build the style context used by the style traversal
        let context = SharedStyleContext {
            traversal_flags: TraversalFlags::empty(),
            stylist: &self.stylist,
            options: GLOBAL_STYLE_DATA.options.clone(),
            guards,
            visited_styles_enabled: false,
            animations: self.animations.clone(),
            current_time_for_animations: now,
            snapshot_map: &self.snapshots,
            registered_speculative_painters: &RegisteredPaintersImpl,
        };

        // components/layout_2020/lib.rs:983
        let root = BlitzNode(self.root_element());
        // dbg!(root);
        let token = RecalcStyle::pre_traverse(root, &context);

        let mut nodes_needing_style_image_flush = Vec::new();
        if token.should_traverse() {
            // Style the elements, resolving their data
            let mut traverser = RecalcStyle::new(context);
            // `Sequential` bypasses Stylo's global pool. See `StyleThreading`.
            let parallel = matches!(self.style_threading, StyleThreading::Parallel);
            let _traversal_lock = parallel.then(|| {
                PARALLEL_TRAVERSAL_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
            });
            let pool_guard = parallel.then(|| STYLE_THREAD_POOL.pool());
            let rayon_pool = pool_guard.as_ref().and_then(|g| g.as_ref());
            style::driver::traverse_dom(&traverser, token, rayon_pool);
            nodes_needing_style_image_flush =
                std::mem::take(traverser.nodes_needing_style_image_flush.get_mut().unwrap());
            self.size_container_seen |= *traverser.saw_size_container.get_mut();
            // A subtree that becomes `display: none` skips the out-of-flow pass.
            // Clear its hoisted lists so rounding won't visit boxes from the
            // last visible layout that have lost their styles or been removed.
            for node_id in traverser.newly_hidden_nodes.into_inner().unwrap() {
                self.clear_subtree_hoisted_children(node_id);
            }
        }
        self.pending_style_image_nodes
            .extend(nodes_needing_style_image_flush);

        for opaque in self.snapshots.keys() {
            if let Some(id) = self.nodes.id_from_opaque(opaque.id()) {
                self.nodes[id].set_has_snapshot(false);
            }
        }
        self.snapshots.clear();

        let mut sets = self.animations.sets.write();
        for set in sets.values_mut() {
            set.clear_canceled_animations();
            for animation in set.animations.iter_mut() {
                animation.is_new = false;
            }
            for transition in set.transitions.iter_mut() {
                transition.is_new = false;
            }
        }
        let mut finished = Vec::new();
        sets.retain(|key, state| {
            let keep = !state.is_empty();
            if !keep {
                finished.push(key.node);
            }
            keep
        });
        // Clear `may_have_animations` on elements whose last set (including
        // their pseudo-elements' sets) was just removed.
        for node in finished {
            if sets.keys().any(|key| key.node == node) {
                continue;
            }
            let node_id = NodeId::from_u64(node.id() as u64);
            if let Some(data) = self.nodes.get(node_id).and_then(|node| node.element_data()) {
                data.set_may_have_animations(false);
            }
        }
        self.has_active_animations = sets.values().any(|state| state.needs_animation_ticks());

        // Maybe run garbage collection. Stylo has internal to determine whether to run or not.
        self.stylist.rule_tree().maybe_gc();

        style::thread_state::exit(ThreadState::LAYOUT);
    }

    /// Compute the style of an element which the regular style traversal
    /// skipped because it lives inside a `display: none` subtree.
    ///
    /// The style traversal culls `display: none` subtrees, so such elements
    /// never have styles stored on them, but `getComputedStyle()` must still
    /// return their computed values. This resolves the style on demand
    /// (styling any unstyled ancestors along the way) without storing it on the
    /// node. Returns `None` for non-elements and elements outside the document.
    pub fn resolve_undisplayed_style(&self, node_id: NodeId) -> Option<Arc<ComputedValues>> {
        let node = self.nodes.get(node_id)?;
        if !node.is_element() || !node.flags.is_in_document() {
            return None;
        }

        style::thread_state::enter(ThreadState::LAYOUT);

        let guard = &self.guard;
        let guards = StylesheetGuards {
            author: &guard.read(),
            ua_or_user: &guard.read(),
        };
        let shared = SharedStyleContext {
            traversal_flags: TraversalFlags::empty(),
            stylist: &self.stylist,
            options: GLOBAL_STYLE_DATA.options.clone(),
            guards,
            visited_styles_enabled: false,
            animations: self.animations.clone(),
            current_time_for_animations: 0.0,
            snapshot_map: &self.snapshots,
            registered_speculative_painters: &RegisteredPaintersImpl,
        };
        // The thread-local context's sequential task list asserts the LAYOUT state when dropped
        let styles = {
            let mut thread_local = ThreadLocalStyleContext::new();
            let mut context = StyleContext {
                shared: &shared,
                thread_local: &mut thread_local,
            };
            resolve_style(
                &mut context,
                BlitzNode(node),
                RuleInclusion::All,
                None,
                None,
            )
        };

        style::thread_state::exit(ThreadState::LAYOUT);

        Some(styles.primary().clone())
    }

    /// Run stylo's relative-selector invalidator over this pass's snapshots so
    /// a `:has()` anchor is recascaded when its subject changes. Gecko drives
    /// this from C++ `RestyleManager`; blitz never did, so the anchor was only
    /// recascaded when it happened to fall inside the blunt `restyle_subtree()`
    /// that `set_attribute` stamped on the mutated node's parent.
    ///
    /// Must run before `traverse_dom`: `invalidated_self` writes RESTYLE_SELF
    /// into the anchor's `ElementData`, and the mutation already marked the
    /// root-to-node spine `dirty_descendants`, so the traversal reaches every
    /// ancestor and every sibling of an ancestor with no extra marking.
    fn invalidate_relative_selectors(&self) {
        if !self.has_relative_selectors() {
            return;
        }
        let quirks_mode = self.stylist.quirks_mode();

        for (opaque, snapshot) in self.snapshots.iter() {
            let Some(node) = self
                .nodes
                .id_from_opaque(opaque.id())
                .map(|id| &self.nodes[id])
            else {
                continue;
            };
            let Some(element) = TNode::as_element(&BlitzNode(node)) else {
                continue;
            };

            // The snapshot holds the pre-mutation state, so the XOR is exactly
            // the bits a `:has(:hover)`-shaped dependency can have flipped.
            let state_changes = snapshot
                .state
                .map_or_else(ElementState::empty, |state| state ^ element.state());
            let has_attrs = snapshot.has_attrs();
            let snapshot_id = if has_attrs { snapshot.id_attr() } else { None };

            let snapshots = Snapshots::new(&self.snapshots);
            RelativeSelectorInvalidator {
                element,
                quirks_mode,
                snapshots: Some(&snapshots),
                invalidated: |_, _| {},
                sibling_traversal_map: SiblingTraversalMap::default(),
            }
            .invalidate_relative_selectors_for_this(
                &self.stylist,
                |element, scope, data, quirks_mode, collector| {
                    let map = data.relative_selector_invalidation_map();

                    // A key the mutation removed is already gone from the
                    // element but still in the snapshot, and vice versa.

                    // Both sides have to be looked up, or the anchor latches on
                    // and never invalidates back when the subject goes away.
                    for id in element.id().into_iter().chain(snapshot_id) {
                        for dep in map
                            .id_to_selector
                            .get(id, quirks_mode)
                            .into_iter()
                            .flatten()
                        {
                            collector.add_dependency(dep, *element, scope);
                        }
                    }

                    let mut each_class = |class: &AtomIdent| {
                        for dep in map
                            .class_to_selector
                            .get(class, quirks_mode)
                            .into_iter()
                            .flatten()
                        {
                            collector.add_dependency(dep, *element, scope);
                        }
                    };
                    element.each_class(&mut each_class);
                    if has_attrs {
                        snapshot.each_class(&mut each_class);
                    }

                    let mut each_attr = |name: &GenericAtomIdent<LocalNameStaticSet>| {
                        for dep in map
                            .other_attribute_affecting_selectors
                            .get(name)
                            .into_iter()
                            .flatten()
                        {
                            collector.add_dependency(dep, *element, scope);
                        }
                    };
                    element.each_attr_name(&mut each_attr);

                    // Unguarded on purpose, unlike id_attr/each_class above:
                    // those are trait methods whose `get_attr` unwraps
                    // `attrs` and would panic on a state-only snapshot, while
                    // this one just walks the `changed_attrs` vec.
                    snapshot.each_attr_changed(&mut each_attr);

                    if state_changes.is_empty() {
                        return;
                    }
                    map.state_affecting_selectors.lookup_with_additional(
                        *element,
                        quirks_mode,
                        None,
                        &[],
                        state_changes,
                        |dep| {
                            if dep.state.intersects(state_changes) {
                                collector.add_dependency(&dep.dep, *element, scope);
                            }
                            true
                        },
                    );
                },
            );
        }
    }

    /// Cheap gate for both relative-selector passes: a document with no
    /// `:has()` anywhere should not pay for the invalidator at all.
    ///
    /// `iter_origins` only sees document-level origins, so any shadow root
    /// forces the slow path rather than risk missing a shadow `:has()`.
    pub(crate) fn has_relative_selectors(&self) -> bool {
        !self.shadow_roots.is_empty()
            || self
                .stylist
                .iter_origins()
                .any(|(data, _)| data.relative_invalidation_map_attributes().used)
    }

    /// Structural sibling of the pass above, for tree mutations rather than
    /// attribute and state ones. Snapshots only exist for the latter, so an
    /// insert or remove has to invalidate at mutation time.
    ///
    /// Ordering is forced by the collector reading the node's live parent and
    /// siblings: insertions invalidate after the node is linked, removals
    /// before it is unlinked.
    fn invalidate_relative_selectors_for_dom_mutation(
        &self,
        node_id: NodeId,
        operation: DomMutationOperation,
    ) {
        if !self.has_relative_selectors() {
            return;
        }
        let Some(node) = self.nodes.get(node_id) else {
            return;
        };
        let Some(element) = TNode::as_element(&BlitzNode(node)) else {
            return;
        };

        // A node that never took part in selector matching carries no search
        // direction, and `DomMutationOperation::accept` rejects those outright.

        // Seeding it from the parent is what makes a freshly created element
        // invalidate at all, and mirrors what matching would have stamped.
        let inherited_search_path = selectors::Element::parent_element(&element)
            .map_or_else(ElementSelectorFlags::empty, |parent| {
                parent.relative_selector_search_direction()
            });

        let snapshots = Snapshots::new(&self.snapshots);
        RelativeSelectorInvalidator {
            element,
            quirks_mode: self.stylist.quirks_mode(),
            snapshots: Some(&snapshots),
            // The invalidator propagates `dirty_descendants` from what it
            // restyles up to the anchor only. `set_dirty_descendants` does not
            // mark ancestors, so the anchor's own ancestors stay clean
            // unless this marks them. A removal runs this before unlinking, and
            // its own `mark_ancestors_dirty` from the parent then stops at the
            // already-set anchor, so the traversal never reached the subject.
            invalidated: |anchor, result| {
                if result.has_invalidated_self()
                    || result.has_invalidated_descendants()
                    || result.has_invalidated_siblings()
                {
                    anchor.mark_ancestors_dirty();
                }
            },
            sibling_traversal_map: SiblingTraversalMap::default(),
        }
        // `subtree`: the node may carry a whole subtree containing the
        // subject, and upstream still gates the walk on the map's own flag.
        .invalidate_relative_selectors_for_dom_mutation(
            true,
            &self.stylist,
            inherited_search_path,
            operation,
        );
    }

    pub(crate) fn invalidate_relative_selectors_for_removal(&self, node_id: NodeId) {
        self.invalidate_relative_selectors_for_dom_mutation(node_id, DomMutationOperation::Remove);
    }

    pub(crate) fn invalidate_relative_selectors_for_insertion(&self, node_id: NodeId) {
        // `Append` only unlocks an optimization that assumes a later sibling
        // can take over the match, so `Insert` is the conservative choice.
        self.invalidate_relative_selectors_for_dom_mutation(node_id, DomMutationOperation::Insert);
    }
}

/// Handle custom painters like images for layouting
///
/// todo: actually implement this
pub struct RegisteredPaintersImpl;
impl RegisteredSpeculativePainters for RegisteredPaintersImpl {
    fn get(&self, _name: &Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

pub struct RecalcStyle<'a> {
    context: SharedStyleContext<'a>,
    /// Nodes whose `background-image`/`mask-image` layers need flushing to
    /// dedicated storage on the node (see `flush_image_layers_from_style`)
    /// because their style changed during this traversal.
    nodes_needing_style_image_flush: Mutex<Vec<NodeId>>,
    newly_hidden_nodes: Mutex<Vec<NodeId>>,
    /// Whether this traversal styled any element or eager pseudo-element as a query container.
    saw_size_container: AtomicBool,
}

impl<'a> RecalcStyle<'a> {
    pub fn new(context: SharedStyleContext<'a>) -> Self {
        RecalcStyle {
            context,
            nodes_needing_style_image_flush: Mutex::new(Vec::new()),
            newly_hidden_nodes: Mutex::new(Vec::new()),
            saw_size_container: AtomicBool::new(false),
        }
    }
}

fn is_size_container_style(style: &ComputedValues) -> bool {
    !style.get_box().slow_clone_container_type().is_normal()
}

/// Whether this element or one of its eager pseudo-elements styles as a query container.
fn styles_a_size_container(styles: &style::data::ElementStyles) -> bool {
    let pseudos = styles.pseudos.as_optional_array();
    styles
        .get_primary()
        .is_some_and(|s| is_size_container_style(s))
        || pseudos.is_some_and(|styles| styles.iter().flatten().any(|s| is_size_container_style(s)))
}

#[allow(unsafe_code)]
impl<'dom> DomTraversal<BlitzNode<'dom>> for RecalcStyle<'_> {
    fn process_preorder<F: FnMut(BlitzNode<'dom>)>(
        &self,
        context: &mut StyleContext<BlitzNode<'dom>>,
        node: BlitzNode<'dom>,
        note_child: F,
    ) {
        if let Some(el) = node.as_element() {
            // let mut data = el.mutate_data().unwrap();
            let mut data = unsafe { el.ensure_data() };
            let was_displayed =
                data.styles.get_primary().is_some() && !data.styles.is_display_none();
            let had_inline_background = data
                .styles
                .get_primary()
                .map(|style| has_inline_background(style));
            recalc_style_at(self, context, el, &mut data, note_child);

            // Parley captures background-fragment presence at box construction. Stylo's repaint
            // damage does not call compute_layout_damage, so handle transparency changes here.
            if let (Some(had_background), Some(style)) =
                (had_inline_background, data.styles.get_primary())
                && had_background != has_inline_background(style)
            {
                data.damage.insert(CONSTRUCT_BOX);
            }

            if was_displayed && data.styles.is_display_none() {
                self.newly_hidden_nodes.lock().unwrap().push(el.0.id());
            }

            // Box construction copies a shown placeholder's look onto its text input, and stylo's
            // damage covers only the control's own style.
            let styled = !data
                .flags
                .contains(ElementDataFlags::TRAVERSED_WITHOUT_STYLING);
            if styled && placeholder_look_is_stale(&self.context, el.0, &data) {
                data.damage.insert(CONSTRUCT_BOX);
            }

            sync_pseudo_element_styles(
                el.0,
                &data,
                &self.nodes_needing_style_image_flush,
                &self.newly_hidden_nodes,
            );
            if let Some(style) = data.styles.get_primary() {
                el.record_out_of_flow(style);
            }

            if !self.saw_size_container.load(Ordering::Relaxed)
                && styles_a_size_container(&data.styles)
            {
                self.saw_size_container.store(true, Ordering::Relaxed);
            }

            if !data.damage.is_empty() {
                // Mark the ancestor chain so that the damage propagation pass
                // visits this element.
                el.mark_damaged();

                if data
                    .styles
                    .get_primary()
                    .is_some_and(|style| needs_style_image_flush(el.0, style))
                {
                    self.nodes_needing_style_image_flush
                        .lock()
                        .unwrap()
                        .push(el.0.id());
                }
            }

            // Gets set later on
            el.0.unset_dirty_descendants();
        }
    }

    #[inline]
    fn needs_postorder_traversal() -> bool {
        false
    }

    fn process_postorder(
        &self,
        _style_context: &mut StyleContext<BlitzNode<'dom>>,
        _node: BlitzNode<'dom>,
    ) {
        panic!("this should never be called")
    }

    #[inline]
    fn shared_context(&self) -> &SharedStyleContext<'_> {
        &self.context
    }
}

/// Whether a text control showing its placeholder holds a `::placeholder`
/// colour or font style that its fresh style no longer gives: a class or an
/// attr() that only the pseudo reads changes nothing stylo diffs.
fn placeholder_look_is_stale(
    context: &SharedStyleContext,
    el: &Node,
    data: &style::data::ElementData,
) -> bool {
    use crate::layout::construct::{placeholder_look, placeholder_pseudo_style};
    let Some(input) = el.element_data().and_then(|e| e.text_input_data()) else {
        return false;
    };
    if !input.is_showing_placeholder {
        return false;
    }
    let Some(primary) = data.styles.get_primary() else {
        return false;
    };
    let fresh = placeholder_pseudo_style(context.stylist, &context.guards, el, primary)
        .map(|style| placeholder_look(&style));
    fresh != input.placeholder_color.zip(input.placeholder_font_style)
}

/// Flush updated pseudo-element (`::before`/`::after`) styles from the owning
/// element's freshly-computed stylo data to the pseudo-element's anonymous node.
///
/// Pseudo-element styles are normally flushed to the pseudo-element's node
/// during box construction (see `flush_pseudo_elements`), but in incremental
/// mode box construction only runs for nodes with construction damage.
/// Pseudo-element style changes which don't require reconstruction (e.g.
/// animations/transitions of repaint- or relayout-only properties) must still
/// be flushed to the pseudo-element's node - along with the damage they imply -
/// so that layout and paint see the new style.
///
/// This runs during the style traversal, immediately after the owning element
/// has been restyled, because that is where the old and new pseudo styles can
/// be diffed precisely (the pseudo node's stored primary style is the "old"
/// style; the owner's just-computed pseudo styles are the "new" ones).
///
/// SAFETY: the style traversal has exclusive access to the tree, and each
/// pseudo-element node is only ever accessed from its owning element's
/// `process_preorder` (pseudo nodes are not part of the DOM child list and are
/// not themselves visited by the traversal), so mutating the pseudo node's
/// stylo data here cannot race with any other traversal thread.
#[allow(unsafe_code)]
fn sync_pseudo_element_styles(
    el: &Node,
    data: &style::data::ElementData,
    nodes_needing_style_image_flush: &Mutex<Vec<NodeId>>,
    newly_hidden_nodes: &Mutex<Vec<NodeId>>,
) {
    let before_node_id = el.before();
    let after_node_id = el.after();
    if before_node_id.is_none() && after_node_id.is_none() {
        return;
    }

    // Note: yes these are kinda backwards (see `flush_pseudo_elements`)
    let pseudos = data.styles.pseudos.as_array();
    let before_style = pseudos[1].clone();
    let after_style = pseudos[0].clone();

    // Creation and removal of pseudo-elements is handled during box construction
    // (Stylo generates construction damage for those cases), so only the case
    // where the pseudo-element both was and remains present is handled here.
    for (pe_node_id, pe_style) in [(before_node_id, before_style), (after_node_id, after_style)] {
        let (Some(pe_node_id), Some(pe_style)) = (pe_node_id, pe_style) else {
            continue;
        };
        let pe_node = el.with(pe_node_id);
        let Some(stylo_data) = pe_node.try_stylo_element_data() else {
            continue;
        };
        let mut pe_data = match unsafe { stylo_data.unsafe_stylo_only_mut() } {
            Some(data) => data,
            None => continue,
        };
        let Some(old_style) = pe_data.styles.primary.clone() else {
            continue;
        };
        if std::ptr::eq(&*old_style, &*pe_style) {
            continue;
        }

        let mut diff = RestyleDamage::compute_style_difference::<BlitzNode>(&old_style, &pe_style);
        if has_inline_background(&old_style) != has_inline_background(&pe_style) {
            diff.damage.insert(CONSTRUCT_BOX);
        }
        if !old_style.get_box().get_display().is_none()
            && pe_style.get_box().get_display().is_none()
        {
            newly_hidden_nodes.lock().unwrap().push(pe_node_id);
        }
        if !diff.damage.is_empty() {
            pe_data.damage.insert(diff.damage);
            pe_node.mark_damaged();

            if needs_style_image_flush(pe_node, &pe_style) {
                nodes_needing_style_image_flush
                    .lock()
                    .unwrap()
                    .push(pe_node_id);
            }
        }
        pe_node.record_out_of_flow(&pe_style);
        pe_data.styles.primary = Some(pe_style);
        pe_data.set_restyled();
    }
}

/// Whether a node's `background-image`/`mask-image` layers need flushing to
/// dedicated storage on the node: the URLs referenced by the new style differ
/// from the image data currently stored on the node.
fn needs_style_image_flush(node: &Node, style: &ComputedValues) -> bool {
    use crate::node::ImageResourceData;
    use style::url::ComputedUrl;
    use style::values::computed::image::Image;

    fn out_of_sync(style_images: &[Image], stored: &[Option<ImageResourceData>]) -> bool {
        if style_images.len() != stored.len() {
            // Differing lengths only matter if either side references an image
            // (a style list of `none` layers and empty storage are in sync).
            return style_images
                .iter()
                .any(|image| matches!(image, Image::Url(_)))
                || stored.iter().any(Option::is_some);
        }
        std::iter::zip(style_images, stored).any(|(style_image, stored)| {
            match (style_image, stored) {
                (Image::Url(ComputedUrl::Valid(url)), Some(data)) => **url != *data.url,
                (Image::Url(ComputedUrl::Valid(_)), None) => true,
                (_, Some(_)) => true,
                (_, None) => false,
            }
        })
    }

    node.data.downcast_element().is_some_and(|el| {
        out_of_sync(
            &style.get_background().background_image.0,
            el.background_images(),
        ) || out_of_sync(&style.get_svg().mask_image.0, el.mask_images())
    })
}
