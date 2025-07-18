//! A Dom that can sync with the VirtualDom mutations intended for use in lazy renderers.

use std::{
    any::TypeId,
    ops::{
        Deref,
        DerefMut,
    },
    sync::Arc,
};

use rustc_hash::{
    FxHashMap,
    FxHashSet,
};
use shipyard::{
    borrow::Borrow,
    error::GetStorage,
    scheduler::ScheduledWorkload,
    track::Untracked,
    Component,
    Get,
    SystemModificator,
    Unique,
    View,
    ViewMut,
    Workload,
    World,
};

use crate::{
    events::EventName,
    node::{
        ElementNode,
        FromAnyValue,
        NodeType,
        OwnedAttributeValue,
    },
    node_ref::{
        NodeMask,
        NodeMaskBuilder,
    },
    passes::{
        Dependant,
        DirtyNodeStates,
        PassDirection,
        TypeErasedState,
    },
    prelude::{
        AttributeMaskBuilder,
        AttributeName,
    },
    tags::TagName,
    tree::{
        TreeMut,
        TreeMutView,
        TreeRef,
        TreeRefView,
    },
    NodeId,
    SendAnyMap,
};

/// The context passes can receive when they are executed
#[derive(Unique)]
pub(crate) struct SendAnyMapWrapper(SendAnyMap);

impl Deref for SendAnyMapWrapper {
    type Target = SendAnyMap;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The nodes that have been marked as dirty in the RealDom
pub(crate) struct NodesDirty<V: FromAnyValue + Send + Sync> {
    passes_updated: FxHashMap<NodeId, FxHashSet<TypeId>>,
    nodes_updated: FxHashMap<NodeId, NodeMask>,
    nodes_created: FxHashSet<NodeId>,
    pub(crate) passes: Box<[TypeErasedState<V>]>,
}

impl<V: FromAnyValue + Send + Sync> NodesDirty<V> {
    /// Mark a node as dirty
    fn mark_dirty(&mut self, node_id: NodeId, mask: NodeMask) {
        self.passes_updated.entry(node_id).or_default().extend(
            self.passes
                .iter()
                .filter_map(|x| x.mask.overlaps(&mask).then_some(x.this_type_id)),
        );
        let nodes_updated = &mut self.nodes_updated;
        if let Some(node) = nodes_updated.get_mut(&node_id) {
            *node = node.union(&mask);
        } else {
            nodes_updated.insert(node_id, mask);
        }
    }

    /// Mark a node that has had a parent changed
    fn mark_parent_added_or_removed(&mut self, node_id: NodeId) {
        let hm = self.passes_updated.entry(node_id).or_default();
        for pass in &*self.passes {
            // If any of the states in this node depend on the parent then mark them as dirty
            for &pass in &pass.parent_dependancies_ids {
                hm.insert(pass);
            }
        }
    }

    /// Mark a node as having a child added or removed
    fn mark_child_changed(&mut self, node_id: NodeId) {
        let hm = self.passes_updated.entry(node_id).or_default();
        for pass in &*self.passes {
            // If any of the states in this node depend on the children then mark them as dirty
            for &pass in &pass.child_dependancies_ids {
                hm.insert(pass);
            }
        }
    }
}

/// A Dom that can sync with the VirtualDom mutations intended for use in lazy renderers.
/// The render state passes from parent to children and or accumulates state from children to parents.
/// To get started:
/// 1) Implement [crate::passes::State] for each part of your state that you want to compute incrementally
/// 2) Create a RealDom [RealDom::new], passing in each state you created
/// 3) Update the state of the RealDom by adding and modifying nodes
/// 4) Call [RealDom::update_state] to update the state of incrementally computed values on each node
///
/// # Custom attribute values
/// To allow custom values to be passed into attributes implement FromAnyValue on a type that can represent your custom value and specify the V generic to be that type. If you have many different custom values, it can be useful to use a enum type to represent the variants.
pub struct RealDom<V: FromAnyValue + Send + Sync = ()> {
    pub(crate) world: World,
    nodes_listening: FxHashMap<EventName, FxHashSet<NodeId>>,
    pub(crate) dirty_nodes: NodesDirty<V>,
    workload: ScheduledWorkload,
    root_id: NodeId,
    phantom: std::marker::PhantomData<V>,
}

impl<V: FromAnyValue + Send + Sync> RealDom<V> {
    /// Create a new RealDom with the given states that will be inserted and updated when needed
    pub fn new(tracked_states: impl Into<Box<[TypeErasedState<V>]>>) -> RealDom<V> {
        let mut tracked_states = tracked_states.into();
        // resolve dependants for each pass
        for i in 1..=tracked_states.len() {
            let (before, after) = tracked_states.split_at_mut(i);
            let (current, before) = before.split_last_mut().unwrap();
            for state in before.iter_mut().chain(after.iter_mut()) {
                let dependants = Arc::get_mut(&mut state.dependants).unwrap();

                let current_dependant = Dependant {
                    type_id: current.this_type_id,
                    enter_shadow_dom: current.enter_shadow_dom,
                };

                // If this node depends on the other state as a parent, then the other state should update its children of the current type when it is invalidated
                if current
                    .parent_dependancies_ids
                    .contains(&state.this_type_id)
                    && !dependants.child.contains(&current_dependant)
                {
                    dependants.child.push(current_dependant);
                }
                // If this node depends on the other state as a child, then the other state should update its parent of the current type when it is invalidated
                if current.child_dependancies_ids.contains(&state.this_type_id)
                    && !dependants.parent.contains(&current_dependant)
                {
                    dependants.parent.push(current_dependant);
                }
                // If this node depends on the other state as a sibling, then the other state should update its siblings of the current type when it is invalidated
                if current.node_dependancies_ids.contains(&state.this_type_id)
                    && !dependants.node.contains(&current.this_type_id)
                {
                    dependants.node.push(current.this_type_id);
                }
            }
            // If the current state depends on itself, then it should update itself when it is invalidated
            let dependants = Arc::get_mut(&mut current.dependants).unwrap();
            let current_dependant = Dependant {
                type_id: current.this_type_id,
                enter_shadow_dom: current.enter_shadow_dom,
            };
            match current.pass_direction {
                PassDirection::ChildToParent => {
                    if !dependants.parent.contains(&current_dependant) {
                        dependants.parent.push(current_dependant);
                    }
                }
                PassDirection::ParentToChild => {
                    if !dependants.child.contains(&current_dependant) {
                        dependants.child.push(current_dependant);
                    }
                }
                _ => {}
            }
        }
        let workload = construct_workload(&mut tracked_states);
        let (workload, _) = workload.build().unwrap();
        let mut world = World::new();
        let root_node: NodeType<V> = NodeType::Element(ElementNode {
            tag: TagName::Root,
            attributes: FxHashMap::default(),
            listeners: FxHashSet::default(),
        });
        let root_id: NodeId = world.add_entity(root_node).into();
        {
            let mut tree: TreeMutView = world.borrow::<TreeMutView>().unwrap();
            tree.create_node(root_id);
        }

        let mut passes_updated = FxHashMap::default();
        let mut nodes_updated = FxHashMap::default();

        passes_updated.insert(
            root_id,
            tracked_states.iter().map(|x| x.this_type_id).collect(),
        );
        nodes_updated.insert(root_id, NodeMaskBuilder::ALL.build());

        RealDom {
            world,
            nodes_listening: FxHashMap::default(),
            dirty_nodes: NodesDirty {
                passes_updated,
                nodes_updated,
                passes: tracked_states,
                nodes_created: [root_id].into_iter().collect(),
            },
            workload,
            root_id,
            phantom: std::marker::PhantomData,
        }
    }

    pub fn deep_clone_node(&mut self, node_id: NodeId) -> NodeMut<V> {
        let clone_id = self.get_mut(node_id).unwrap().clone_node();
        self.get_mut(clone_id).unwrap()
    }

    /// Get a reference to the tree.
    pub fn tree_ref(&self) -> TreeRefView {
        self.world.borrow::<TreeRefView>().unwrap()
    }

    /// Get a mutable reference to the tree.
    pub fn tree_mut(&self) -> TreeMutView {
        self.world.borrow::<TreeMutView>().unwrap()
    }

    /// Create a new node of the given type in the dom and return a mutable reference to it.
    pub fn create_node(&mut self, node: impl Into<NodeType<V>>) -> NodeMut<'_, V> {
        let node = node.into();

        let id = self.world.add_entity(node).into();
        self.tree_mut().create_node(id);

        self.dirty_nodes
            .passes_updated
            .entry(id)
            .or_default()
            .extend(self.dirty_nodes.passes.iter().map(|x| x.this_type_id));
        self.dirty_nodes
            .mark_dirty(id, NodeMaskBuilder::ALL.build());
        self.dirty_nodes.nodes_created.insert(id);

        NodeMut::new(id, self)
    }

    pub fn is_node_listening(&self, node_id: &NodeId, event: &EventName) -> bool {
        self.nodes_listening
            .get(event)
            .map(|listeners| listeners.contains(node_id))
            .unwrap_or_default()
    }

    pub fn get_listeners(&self, event: &EventName) -> Vec<NodeRef<V>> {
        if let Some(nodes) = self.nodes_listening.get(event) {
            nodes
                .iter()
                .map(|id| NodeRef { id: *id, dom: self })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Returns the id of the root node.
    pub fn root_id(&self) -> NodeId {
        self.root_id
    }

    /// Check if a node exists in the dom.
    pub fn contains(&self, id: NodeId) -> bool {
        self.tree_ref().contains(id)
    }

    /// Get a reference to a node.
    pub fn get(&self, id: NodeId) -> Option<NodeRef<'_, V>> {
        self.contains(id).then_some(NodeRef { id, dom: self })
    }

    /// Get a mutable reference to a node.
    pub fn get_mut(&mut self, id: NodeId) -> Option<NodeMut<'_, V>> {
        let contains = self.contains(id);
        contains.then(|| NodeMut::new(id, self))
    }

    /// Borrow a component from the world without updating the dirty nodes.
    #[inline(always)]
    fn borrow_raw<'a, B: Borrow>(&'a self) -> Result<B, GetStorage>
    where
        B::View<'a>: Borrow<View<'a> = B>,
    {
        self.world.borrow::<B::View<'a>>()
    }

    /// Borrow a component from the world without updating the dirty nodes.
    fn borrow_node_type_mut(&self) -> Result<ViewMut<NodeType<V>>, GetStorage> {
        self.world.borrow::<ViewMut<NodeType<V>>>()
    }

    /// Update the state of the dom, after appling some mutations. This will keep the nodes in the dom up to date with their VNode counterparts.
    pub fn update_state(&mut self, ctx: SendAnyMap) -> FxHashMap<NodeId, NodeMask> {
        let passes = std::mem::take(&mut self.dirty_nodes.passes_updated);
        let nodes_updated = std::mem::take(&mut self.dirty_nodes.nodes_updated);

        let dirty_nodes =
            DirtyNodeStates::with_passes(self.dirty_nodes.passes.iter().map(|p| p.this_type_id));
        let tree = self.tree_ref();
        for (node_id, passes) in passes {
            // remove any nodes that were created and then removed in the same mutations from the dirty nodes list
            if let Some(height) = tree.height(node_id) {
                for pass in passes {
                    dirty_nodes.insert(pass, node_id, height);
                }
            }
        }

        let _ = self.world.remove_unique::<DirtyNodeStates>();
        let _ = self.world.remove_unique::<SendAnyMapWrapper>();
        self.world.add_unique(dirty_nodes);
        self.world.add_unique(SendAnyMapWrapper(ctx));

        self.workload.run_with_world(&self.world).unwrap();

        nodes_updated
    }

    /// Traverses the dom in a depth first manner,
    /// calling the provided function on each node only when the parent function returns `true`.
    /// This is useful to not traverse through text nodes for instance.
    pub fn traverse_depth_first_advanced(&self, mut f: impl FnMut(NodeRef<V>) -> bool) {
        let mut stack = vec![self.root_id()];
        let tree = self.tree_ref();
        while let Some(id) = stack.pop() {
            if let Some(node) = self.get(id) {
                let traverse_children = f(node);
                if traverse_children {
                    let children = tree.children_ids_advanced(id, false);
                    stack.extend(children.iter().copied().rev());
                }
            }
        }
    }

    /// Traverses the dom in a depth first manner, calling the provided function on each node.
    pub fn traverse_depth_first(&self, mut f: impl FnMut(NodeRef<V>)) {
        self.traverse_depth_first_advanced(move |node| {
            f(node);
            true
        })
    }

    /// Returns a reference to the underlying world. Any changes made to the world will not update the reactive system.
    pub fn raw_world(&self) -> &World {
        &self.world
    }

    /// Returns a mutable reference to the underlying world. Any changes made to the world will not update the reactive system.
    pub fn raw_world_mut(&mut self) -> &mut World {
        &mut self.world
    }
}

/// A reference to a tracked component in a node.
pub struct ViewEntry<'a, V: Component + Send + Sync> {
    view: View<'a, V>,
    id: NodeId,
}

impl<'a, V: Component + Send + Sync> ViewEntry<'a, V> {
    fn new(view: View<'a, V>, id: NodeId) -> Self {
        Self { view, id }
    }
}

impl<V: Component + Send + Sync> Deref for ViewEntry<'_, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        &self.view[*self.id]
    }
}

/// A mutable reference to a tracked component in a node.
pub struct ViewEntryMut<'a, V: Component<Tracking = Untracked> + Send + Sync> {
    view: ViewMut<'a, V, Untracked>,
    id: NodeId,
}

impl<'a, V: Component<Tracking = Untracked> + Send + Sync> ViewEntryMut<'a, V> {
    fn new(view: ViewMut<'a, V, Untracked>, id: NodeId) -> Self {
        Self { view, id }
    }
}

impl<V: Component<Tracking = Untracked> + Send + Sync> Deref for ViewEntryMut<'_, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.view.get(*self.id).unwrap()
    }
}

impl<V: Component<Tracking = Untracked> + Send + Sync> DerefMut for ViewEntryMut<'_, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.view[*self.id]
    }
}

/// A immutable view of a node
pub trait NodeImmutable<V: FromAnyValue + Send + Sync = ()>: Sized {
    /// Get the real dom this node was created in
    fn real_dom(&self) -> &RealDom<V>;

    /// Get the id of the current node
    fn id(&self) -> NodeId;

    /// Get the type of the current node
    #[inline]
    fn node_type(&self) -> ViewEntry<NodeType<V>> {
        self.get().unwrap()
    }

    /// Get a component from the current node
    #[inline(always)]
    fn get<'a, T: Component + Sync + Send>(&'a self) -> Option<ViewEntry<'a, T>> {
        // self.real_dom().tree.get(self.id())
        let view = self.real_dom().world.borrow::<View<'a, T>>().ok()?;
        view.contains(self.id().into())
            .then(|| ViewEntry::new(view, self.id()))
    }

    /// Get the ids of the children of the current node, if enter_shadow_dom is true and the current node is a shadow slot, the ids of the nodes under the node the shadow slot is attached to will be returned
    #[inline]
    fn children_ids_advanced(&self, id: NodeId, enter_shadow_dom: bool) -> Vec<NodeId> {
        self.real_dom()
            .tree_ref()
            .children_ids_advanced(id, enter_shadow_dom)
    }

    /// Get the ids of the children of the current node
    #[inline]
    fn child_ids(&self) -> Vec<NodeId> {
        self.real_dom().tree_ref().children_ids(self.id())
    }

    /// Get the children of the current node
    #[inline]
    fn children(&self) -> Vec<NodeRef<V>> {
        self.child_ids()
            .iter()
            .map(|id| NodeRef {
                id: *id,
                dom: self.real_dom(),
            })
            .collect()
    }

    /// Get the id of the parent of the current node
    #[inline]
    fn parent_id(&self) -> Option<NodeId> {
        self.real_dom().tree_ref().parent_id(self.id())
    }

    /// Get the parent of the current node
    #[inline]
    fn parent(&self) -> Option<NodeRef<V>> {
        self.parent_id().map(|id| NodeRef {
            id,
            dom: self.real_dom(),
        })
    }

    /// Get the height of the current node in the tree (the number of nodes between the current node and the root)
    #[inline]
    fn height(&self) -> u16 {
        self.real_dom().tree_ref().height(self.id()).unwrap()
    }
}

/// An immutable reference to a node in a RealDom
pub struct NodeRef<'a, V: FromAnyValue + Send + Sync = ()> {
    id: NodeId,
    dom: &'a RealDom<V>,
}

impl<V: FromAnyValue + Send + Sync> Clone for NodeRef<'_, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V: FromAnyValue + Send + Sync> Copy for NodeRef<'_, V> {}

impl<V: FromAnyValue + Send + Sync> NodeImmutable<V> for NodeRef<'_, V> {
    #[inline(always)]
    fn real_dom(&self) -> &RealDom<V> {
        self.dom
    }

    #[inline(always)]
    fn id(&self) -> NodeId {
        self.id
    }
}

/// A mutable refrence to a node in the RealDom that tracks what States need to be updated
pub struct NodeMut<'a, V: FromAnyValue + Send + Sync = ()> {
    id: NodeId,
    dom: &'a mut RealDom<V>,
}

impl<'a, V: FromAnyValue + Send + Sync> NodeMut<'a, V> {
    /// Create a new mutable refrence to a node in a RealDom
    pub fn new(id: NodeId, dom: &'a mut RealDom<V>) -> Self {
        Self { id, dom }
    }
}

impl<V: FromAnyValue + Send + Sync> NodeImmutable<V> for NodeMut<'_, V> {
    #[inline(always)]
    fn real_dom(&self) -> &RealDom<V> {
        self.dom
    }

    #[inline(always)]
    fn id(&self) -> NodeId {
        self.id
    }
}

impl<V: FromAnyValue + Send + Sync> NodeMut<'_, V> {
    /// Get the real dom this node was created in mutably
    #[inline(always)]
    pub fn real_dom_mut(&mut self) -> &mut RealDom<V> {
        self.dom
    }

    /// Get a component from the current node mutably
    #[inline]
    pub fn get_mut<T: Component<Tracking = Untracked> + Sync + Send>(
        &mut self,
    ) -> Option<ViewEntryMut<T>> {
        // mark the node state as dirty
        self.dom
            .dirty_nodes
            .passes_updated
            .entry(self.id)
            .or_default()
            .insert(TypeId::of::<T>());
        let view_mut: ViewMut<T> = self.dom.borrow_raw().ok()?;
        view_mut
            .contains(self.id.into())
            .then_some(ViewEntryMut::new(view_mut, self.id))
    }

    /// Insert a custom component into this node
    ///
    /// Note: Components that implement State and are added when the RealDom is created will automatically be created
    #[inline]
    pub fn insert<T: Component + Sync + Send>(&mut self, value: T) {
        // mark the node state as dirty
        self.dom
            .dirty_nodes
            .passes_updated
            .entry(self.id)
            .or_default()
            .insert(TypeId::of::<T>());
        self.dom.world.add_component(self.id.into(), value);
    }

    /// Add the given node to the end of this nodes children
    #[inline]
    pub fn add_child(&mut self, child: NodeId) {
        self.dom.dirty_nodes.mark_child_changed(self.id);
        self.dom.dirty_nodes.mark_parent_added_or_removed(child);
        self.dom.tree_mut().add_child(self.id, child);
    }

    /// Insert this node after the given node
    #[inline]
    pub fn insert_after(&mut self, old: NodeId) {
        let id = self.id();
        let parent_id = { self.dom.tree_ref().parent_id(old) };
        if let Some(parent_id) = parent_id {
            self.dom.dirty_nodes.mark_child_changed(parent_id);
            self.dom.dirty_nodes.mark_parent_added_or_removed(id);
        }
        self.dom.tree_mut().insert_after(old, id);
    }

    /// Insert this node before the given node
    #[inline]
    pub fn insert_before(&mut self, old: NodeId) {
        let id = self.id();
        let parent_id = { self.dom.tree_ref().parent_id(old) };
        if let Some(parent_id) = parent_id {
            self.dom.dirty_nodes.mark_child_changed(parent_id);
            self.dom.dirty_nodes.mark_parent_added_or_removed(id);
        }
        self.dom.tree_mut().insert_before(old, id);
    }

    /// Remove this node from the RealDom
    #[inline]
    pub fn remove(&mut self) {
        let id = self.id();
        {
            let RealDom {
                world,
                nodes_listening,
                ..
            } = &mut self.dom;
            let mut view = world.borrow::<ViewMut<NodeType<V>>>().unwrap();
            if let NodeType::Element(ElementNode { listeners, .. }) = &mut view[id.into()] {
                let listeners = std::mem::take(listeners);
                for event in listeners {
                    nodes_listening.get_mut(&event).unwrap().remove(&id);
                }
            }
        }
        let parent_id = { self.dom.tree_ref().parent_id(id) };
        if let Some(parent_id) = parent_id {
            self.real_dom_mut()
                .dirty_nodes
                .mark_child_changed(parent_id);
        }
        let children_ids = self.child_ids();
        for child in children_ids {
            self.dom.get_mut(child).unwrap().remove();
        }
        self.dom.tree_mut().remove(id);
        self.real_dom_mut().raw_world_mut().delete_entity(id.into());
    }

    /// Add an event listener
    #[inline]
    pub fn add_event_listener(&mut self, event: EventName) {
        let id = self.id();
        let RealDom {
            world,
            dirty_nodes,
            nodes_listening,
            ..
        } = &mut self.dom;
        let mut view = world.borrow::<ViewMut<NodeType<V>>>().unwrap();
        let node_type: &mut NodeType<V> = &mut view[id.into()];
        if let NodeType::Element(ElementNode { listeners, .. }) = node_type {
            dirty_nodes.mark_dirty(self.id, NodeMaskBuilder::new().with_listeners().build());
            listeners.insert(event);
            match nodes_listening.get_mut(&event) {
                Some(hs) => {
                    hs.insert(id);
                }
                None => {
                    let mut hs = FxHashSet::default();
                    hs.insert(id);
                    nodes_listening.insert(event, hs);
                }
            }
        }
    }

    /// Remove an event listener
    #[inline]
    pub fn remove_event_listener(&mut self, event: &EventName) {
        let id = self.id();
        let RealDom {
            world,
            dirty_nodes,
            nodes_listening,
            ..
        } = &mut self.dom;
        let mut view = world.borrow::<ViewMut<NodeType<V>>>().unwrap();
        let node_type: &mut NodeType<V> = &mut view[id.into()];
        if let NodeType::Element(ElementNode { listeners, .. }) = node_type {
            dirty_nodes.mark_dirty(self.id, NodeMaskBuilder::new().with_listeners().build());
            listeners.remove(event);

            nodes_listening.get_mut(event).unwrap().remove(&id);
        }
    }

    /// Get a mutable reference to the type of the current node
    pub fn node_type_mut(&mut self) -> NodeTypeMut<'_, V> {
        let id = self.id();
        let RealDom {
            world, dirty_nodes, ..
        } = &mut self.dom;
        let view = world.borrow::<ViewMut<NodeType<V>>>().unwrap();
        let node_type = ViewEntryMut::new(view, id);
        match &*node_type {
            NodeType::Element(_) => NodeTypeMut::Element(ElementNodeMut {
                id,
                element: node_type,
                dirty_nodes,
            }),
            NodeType::Text(_) => NodeTypeMut::Text(TextNodeMut {
                id,
                text: node_type,
                dirty_nodes,
            }),
            NodeType::Placeholder => NodeTypeMut::Placeholder,
        }
    }

    /// Set the type of the current node
    pub fn set_type(&mut self, new: NodeType<V>) {
        {
            let mut view: ViewMut<NodeType<V>> = self.dom.borrow_node_type_mut().unwrap();
            view[self.id.into()] = new;
        }
        self.dom
            .dirty_nodes
            .mark_dirty(self.id, NodeMaskBuilder::ALL.build())
    }

    /// Clone a node and it's children and returns the id of the new node.
    /// This is more effecient than creating the node from scratch because it can pre-allocate the memory required.
    #[inline]
    pub fn clone_node(&mut self) -> NodeId {
        let new_node = self.node_type().clone();
        let rdom = self.real_dom_mut();
        let new_id = rdom.create_node(new_node).id();

        let children = self.child_ids();
        let children = children.to_vec();
        let rdom = self.real_dom_mut();
        for child in children {
            let child_id = rdom.get_mut(child).unwrap().clone_node();
            rdom.get_mut(new_id).unwrap().add_child(child_id);
        }
        new_id
    }
}

/// A mutable refrence to the type of a node in the RealDom
pub enum NodeTypeMut<'a, V: FromAnyValue + Send + Sync = ()> {
    /// An element node
    Element(ElementNodeMut<'a, V>),
    /// A text node
    Text(TextNodeMut<'a, V>),
    /// A placeholder node
    Placeholder,
}

/// A mutable refrence to a text node in the RealDom
pub struct TextNodeMut<'a, V: FromAnyValue + Send + Sync = ()> {
    id: NodeId,
    text: ViewEntryMut<'a, NodeType<V>>,
    dirty_nodes: &'a mut NodesDirty<V>,
}

impl<V: FromAnyValue + Send + Sync> TextNodeMut<'_, V> {
    /// Get the underlying test of the node
    pub fn text(&self) -> &str {
        match &*self.text {
            NodeType::Text(text) => text,
            _ => unreachable!(),
        }
    }

    /// Get the underlying text mutably
    pub fn text_mut(&mut self) -> &mut String {
        self.dirty_nodes
            .mark_dirty(self.id, NodeMaskBuilder::new().with_text().build());
        match &mut *self.text {
            NodeType::Text(text) => text,
            _ => unreachable!(),
        }
    }
}

impl<V: FromAnyValue + Send + Sync> Deref for TextNodeMut<'_, V> {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        match &*self.text {
            NodeType::Text(text) => text,
            _ => unreachable!(),
        }
    }
}

impl<V: FromAnyValue + Send + Sync> DerefMut for TextNodeMut<'_, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.text_mut()
    }
}

/// A mutable refrence to a text Element node in the RealDom
pub struct ElementNodeMut<'a, V: FromAnyValue + Send + Sync = ()> {
    id: NodeId,
    element: ViewEntryMut<'a, NodeType<V>>,
    dirty_nodes: &'a mut NodesDirty<V>,
}

impl std::fmt::Debug for ElementNodeMut<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElementNodeMut")
            .field("id", &self.id)
            .field("element", &*self.element)
            .finish()
    }
}

impl<V: FromAnyValue + Send + Sync> ElementNodeMut<'_, V> {
    /// Get the current element mutably (does not mark anything as dirty)
    fn element_mut(&mut self) -> &mut ElementNode<V> {
        match &mut *self.element {
            NodeType::Element(element) => element,
            _ => unreachable!(),
        }
    }

    /// Set an attribute in the element
    pub fn set_attribute(
        &mut self,
        name: impl Into<AttributeName>,
        value: impl Into<OwnedAttributeValue<V>>,
    ) -> Option<OwnedAttributeValue<V>> {
        let name = name.into();
        let value = value.into();
        self.dirty_nodes.mark_dirty(
            self.id,
            NodeMaskBuilder::new()
                .with_attrs(AttributeMaskBuilder::Some(&[name]))
                .build(),
        );
        self.element_mut().attributes.insert(name, value)
    }

    /// Remove an attribute from the element
    pub fn remove_attribute(&mut self, name: &AttributeName) -> Option<OwnedAttributeValue<V>> {
        self.dirty_nodes.mark_dirty(
            self.id,
            NodeMaskBuilder::new()
                .with_attrs(AttributeMaskBuilder::Some(&[*name]))
                .build(),
        );
        self.element_mut().attributes.remove(name)
    }

    /// Get an attribute of the element
    pub fn get_attribute_mut(
        &mut self,
        name: &AttributeName,
    ) -> Option<&mut OwnedAttributeValue<V>> {
        self.dirty_nodes.mark_dirty(
            self.id,
            NodeMaskBuilder::new()
                .with_attrs(AttributeMaskBuilder::Some(&[*name]))
                .build(),
        );
        self.element_mut().attributes.get_mut(name)
    }
}

// Create a workload from all of the passes. This orders the passes so that each pass will only run at most once.
fn construct_workload<V: FromAnyValue + Send + Sync>(
    passes: &mut [TypeErasedState<V>],
) -> Workload {
    let mut workload = Workload::new("Main Workload");
    // Assign a unique index to keep track of each pass
    let mut unresloved_workloads = passes
        .iter_mut()
        .enumerate()
        .map(|(i, pass)| {
            let workload = Some(pass.create_workload());
            (i, pass, workload)
        })
        .collect::<Vec<_>>();
    // set all the labels
    for (id, _, workload) in &mut unresloved_workloads {
        *workload = Some(workload.take().unwrap().tag(id.to_string()));
    }
    // mark any dependancies
    for i in 0..unresloved_workloads.len() {
        let (_, pass, _) = &unresloved_workloads[i];
        let all_dependancies: Vec<_> = pass.combined_dependancy_type_ids().collect();
        for ty_id in all_dependancies {
            let &(dependancy_id, _, _) = unresloved_workloads
                .iter()
                .find(|(_, pass, _)| pass.this_type_id == ty_id)
                .unwrap();
            let (_, _, workload) = &mut unresloved_workloads[i];
            *workload = workload
                .take()
                .map(|workload| workload.after_all(dependancy_id.to_string()));
        }
    }
    // Add all of the passes
    for (_, _, mut workload_system) in unresloved_workloads {
        workload = workload.with_system(workload_system.take().unwrap());
    }
    workload
}
