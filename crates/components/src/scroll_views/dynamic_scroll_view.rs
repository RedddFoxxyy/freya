//! High-performance virtualized scroll view for dynamically-sized content.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

use std::cmp::Ordering;

use dioxus::prelude::*;
use freya_elements::{
    self as dioxus_elements,
    events::{keyboard::Key, KeyboardEvent, MouseEvent, WheelEvent},
};
use freya_hooks::{use_applied_theme, use_focus, use_node, ScrollBarThemeWith};

use crate::{
    get_corrected_scroll_position, get_scroll_position_from_cursor, get_scroll_position_from_wheel,
    get_scrollbar_pos_and_size, is_scrollbar_visible, manage_key_event,
    scroll_views::{
        use_scroll_controller::{use_scroll_controller, ScrollConfig},
        Axis, ScrollBar, ScrollThumb,
    },
    ScrollController, SCROLL_SPEED_MULTIPLIER,
};

/// A default height for items that have not been measured yet.
const DEFAULT_ITEM_HEIGHT: f32 = 25.0;

/// A layout cache to store and manage the heights of items.
#[derive(Clone)]
struct LayoutManager {
    /// A vector storing the measured height of each item. `None` if not yet measured.
    heights: Vec<Option<f32>>,
    /// The default height for unmeasured items.
    default_item_height: f32,
}

impl LayoutManager {
    /// Creates a new `LayoutManager`.
    fn new(length: usize, default_item_height: f32) -> Self {
        Self {
            heights: vec![None; length],
            default_item_height,
        }
    }

    /// Gets the height of a specific item, returning the default if not measured.
    fn get_item_height(&self, index: usize) -> f32 {
        self.heights
            .get(index)
            .cloned()
            .flatten()
            .unwrap_or(self.default_item_height)
    }

    /// Updates the measured height of an item.
    fn set_item_height(&mut self, index: usize, height: f32) {
        if let Some(item_height) = self.heights.get_mut(index) {
            *item_height = Some(height);
        }
    }

    // NOTE: This might be required in future.
    fn _update_length(&mut self, new_length: usize) {
        match new_length.cmp(&self.heights.len()) {
            Ordering::Greater => self.heights.resize(new_length, None),
            Ordering::Less => self.heights.truncate(new_length),
            Ordering::Equal => { /* nothing to do */ }
        }
    }

    /// Calculates the total estimated height of all items.
    fn get_total_height(&self) -> f32 {
        self.heights
            .iter()
            .map(|h| h.unwrap_or(self.default_item_height))
            .sum()
    }

    /// Calculates the visible range of items and the offset for the content window.
    fn get_visible_range_and_offset(
        &self,
        scroll_y: f32,
        viewport_height: f32,
        overscan: usize,
    ) -> (std::ops::Range<usize>, f32) {
        if self.heights.is_empty() {
            return (0..0, 0.0);
        }

        let mut y_pos = 0.0;
        let mut start_node = 0;
        let mut content_offset = 0.0;
        let mut found_start = false;

        // Find the start of the visible range
        for (i, height) in self.heights.iter().enumerate() {
            let item_height = height.unwrap_or(self.default_item_height);
            let next_y_pos = y_pos + item_height;

            if next_y_pos >= -scroll_y {
                content_offset = y_pos;
                start_node = i;
                found_start = true;
                break;
            }
            y_pos = next_y_pos;
        }

        if !found_start {
            return (0..0, 0.0);
        }

        // Find the end of the visible range
        let mut end_node = start_node;
        let mut visible_height = 0.0;
        for (i, height) in self.heights.iter().enumerate().skip(start_node) {
            let item_height = height.unwrap_or(self.default_item_height);
            visible_height += item_height;
            end_node = i + 1;
            if visible_height >= viewport_height {
                break;
            }
        }

        // Apply overscan to render items slightly outside the viewport for smoother scrolling
        let start = start_node.saturating_sub(overscan);
        let end = (end_node + overscan).min(self.heights.len());

        // Recalculate content offset based on the new start index with overscan
        let overscan_offset: f32 = (start..start_node).map(|i| self.get_item_height(i)).sum();
        let content_offset = content_offset - overscan_offset;

        (start..end, content_offset)
    }
}

/// A wrapper component to measure the size of its child.
#[component]
fn MeasuredItem(
    children: Element,
    index: usize,
    on_measure: EventHandler<(usize, f32)>,
) -> Element {
    let (node_ref, size) = use_node();
    let mut last_reported_height = use_signal(|| 0.0_f32);

    // When the node's size changes, report it back to the parent.
    use_effect(use_reactive(&size, move |size| {
        let height = size.area.height();
        let last_height = *last_reported_height.read();

        if height > 0.0 && (last_height == 0.0 || (height - last_height).abs() > 2.0) {
            *last_reported_height.write() = height;
            on_measure.call((index, height));
        }
    }));

    rsx!(
        rect {
            reference: node_ref,
            width: "100%",
            height: "auto",
            {children}
        }
    )
}

/// Properties for the [`DynamicVirtualScrollView`] component.
#[derive(Props, Clone)]
pub struct DynamicVirtualScrollViewProps<
    Builder: 'static + Clone + Fn(usize, &Option<BuilderArgs>) -> Element,
    BuilderArgs: Clone + 'static + PartialEq = (),
> {
    /// Width of the container.
    #[props(default = "fill".into())]
    pub width: String,
    /// Height of the container.
    #[props(default = "fill".into())]
    pub height: String,
    /// Padding of the container.
    #[props(default = "0".to_string())]
    pub padding: String,
    /// Theme for the scrollbar.
    pub scrollbar_theme: Option<ScrollBarThemeWith>,
    /// The total number of items.
    pub length: usize,
    /// A function to build a single item.
    pub builder: Builder,
    #[props(into)]
    pub builder_args: Option<BuilderArgs>,
    /// The number of items to render outside the visible viewport.
    #[props(default = 5)]
    pub overscan: usize,
    /// A custom scroll controller.
    pub scroll_controller: Option<ScrollController>,
    /// Show the scrollbar.
    #[props(default = true)]
    pub show_scrollbar: bool,
    /// Enable scrolling with arrow keys.
    #[props(default = true)]
    pub scroll_with_arrows: bool,
    /// If `false` (default), wheel scroll with no shift will scroll vertically no matter the direction.
    /// If `true`, wheel scroll with no shift will scroll horizontally.
    #[props(default = false)]
    pub invert_scroll_wheel: bool,
}

impl<
        BuilderArgs: Clone + PartialEq,
        Builder: Clone + Fn(usize, &Option<BuilderArgs>) -> Element,
    > PartialEq for DynamicVirtualScrollViewProps<Builder, BuilderArgs>
{
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.padding == other.padding
            && self.length == other.length
            && self.overscan == other.overscan
            && self.scroll_controller == other.scroll_controller
            && self.show_scrollbar == other.show_scrollbar
            && self.scroll_with_arrows == other.scroll_with_arrows
            && self.builder_args == other.builder_args // <-- The crucial comparison
    }
}

/// A high-performance scroll view for a large number of items with variable heights.
#[allow(non_snake_case)]
pub fn DynamicVirtualScrollView<
    Builder: Clone + Fn(usize, &Option<BuilderArgs>) -> Element,
    BuilderArgs: Clone + PartialEq,
>(
    DynamicVirtualScrollViewProps {
        width,
        height,
        padding,
        scrollbar_theme,
        length,
        builder,
        builder_args,
        overscan,
        scroll_controller,
        show_scrollbar,
        scroll_with_arrows,
        invert_scroll_wheel,
    }: DynamicVirtualScrollViewProps<Builder, BuilderArgs>,
) -> Element {
    let scroll_controller =
        scroll_controller.unwrap_or_else(|| use_scroll_controller(ScrollConfig::default));
    let mut clicking_shift = use_signal(|| false);
    let mut clicking_alt = use_signal(|| false);
    let (mut scrolled_x, mut scrolled_y) = scroll_controller.into();
    let (node_ref, size) = use_node();
    let mut focus = use_focus();
    let applied_scrollbar_theme = use_applied_theme!(&scrollbar_theme, scroll_bar);

    // State for managing the layout cache
    let mut layout_manager = use_signal(|| LayoutManager::new(length, DEFAULT_ITEM_HEIGHT));
    let mut is_updating = use_signal(|| false);

    use_effect(use_reactive(&builder_args, move |_| {
        // NOTE: When the builder_args change, the underlying content is considered stale.
        // It invalidates all cached heights to force re-measurement.
        let mut manager = layout_manager.write();
        if !manager.heights.is_empty() {
            manager.heights = vec![None; length];
        }
    }));

    let total_content_height = layout_manager.read().get_total_height();
    let viewport_height = size.area.height();

    let corrected_scrolled_y = get_corrected_scroll_position(
        total_content_height,
        viewport_height,
        *scrolled_y.read() as f32,
    );

    let (visible_range, content_offset) = layout_manager.read().get_visible_range_and_offset(
        corrected_scrolled_y,
        viewport_height,
        overscan,
    );

    // Event handler to update the layout cache when an item is measured
    let on_measure = move |(index, height): (usize, f32)| {
        let current_height = layout_manager.read().heights.get(index).cloned().flatten();

        // Only update if height is significantly different or first measurement
        if current_height
            .map(|h| (h - height).abs() > 2.0)
            .unwrap_or(true)
        {
            *is_updating.write() = true;

            let old_total_height = layout_manager.read().get_total_height();
            layout_manager.write().set_item_height(index, height);
            let new_total_height = layout_manager.read().get_total_height();

            // Adjust scroll position to prevent bouncing when content above viewport changes
            if index < visible_range.start && corrected_scrolled_y > 0.0 {
                let height_diff = new_total_height - old_total_height;
                if height_diff.abs() > 1.0 {
                    let new_scroll = (corrected_scrolled_y + height_diff)
                        .max(0.0)
                        .min((new_total_height - viewport_height).max(0.0));

                    *scrolled_y.write() = new_scroll as i32;
                }
            }

            *is_updating.write() = false;
        }
    };

    let mut clicking_scrollbar = use_signal::<Option<(Axis, f64)>>(|| None);

    let onwheel = move |e: WheelEvent| {
        let speed_multiplier = if *clicking_alt.peek() {
            SCROLL_SPEED_MULTIPLIER
        } else {
            1.0
        };

        let invert_direction = (clicking_shift() || invert_scroll_wheel)
            && (!clicking_shift() || !invert_scroll_wheel);

        let (_x_movement, y_movement) = if invert_direction {
            (
                e.get_delta_y() as f32 * speed_multiplier,
                e.get_delta_x() as f32 * speed_multiplier,
            )
        } else {
            (
                e.get_delta_x() as f32 * speed_multiplier,
                e.get_delta_y() as f32 * speed_multiplier,
            )
        };

        let scroll_position_y = get_scroll_position_from_wheel(
            y_movement,
            total_content_height,
            viewport_height,
            corrected_scrolled_y,
        );

        // Only scroll when there is still area to scroll
        if *scrolled_y.peek() != scroll_position_y {
            e.stop_propagation();
            *scrolled_y.write() = scroll_position_y;
        }
    };

    let oncaptureglobalmousemove = move |e: MouseEvent| {
        if let Some((Axis::Y, y)) = *clicking_scrollbar.peek() {
            let coordinates = e.get_element_coordinates();
            let cursor_y = coordinates.y - y - size.area.min_y() as f64;
            let scroll_position = get_scroll_position_from_cursor(
                cursor_y as f32,
                total_content_height,
                viewport_height,
            );
            *scrolled_y.write() = scroll_position;
            e.prevent_default();
            focus.request_focus();
        }
    };

    let onglobalkeydown = move |e: KeyboardEvent| match &e.key {
        Key::Shift => {
            clicking_shift.set(true);
        }
        Key::Alt => {
            clicking_alt.set(true);
        }
        k => {
            if !focus.is_focused() {
                return;
            }

            if !scroll_with_arrows
                && (k == &Key::ArrowUp
                    || k == &Key::ArrowRight
                    || k == &Key::ArrowDown
                    || k == &Key::ArrowLeft)
            {
                return;
            }

            let (x, y) = manage_key_event(
                e,
                (*scrolled_x.read() as f32, corrected_scrolled_y),
                total_content_height,
                0.0,
                viewport_height,
                0.0,
            );
            scrolled_x.set(x as i32);
            scrolled_y.set(y as i32);
        }
    };

    let onglobalkeyup = move |e: KeyboardEvent| {
        if e.key == Key::Shift {
            clicking_shift.set(false);
        } else if e.key == Key::Alt {
            clicking_alt.set(false);
        }
    };

    let onmousedown_y = move |e: MouseEvent| {
        *clicking_scrollbar.write() = Some((Axis::Y, e.get_element_coordinates().y));
    };

    let onglobalclick = move |_: MouseEvent| {
        if clicking_scrollbar.peek().is_some() {
            *clicking_scrollbar.write() = None;
        }
    };

    let (scrollbar_y, scrollbar_height) =
        get_scrollbar_pos_and_size(total_content_height, viewport_height, corrected_scrolled_y);

    let vertical_scrollbar_is_visible =
        is_scrollbar_visible(show_scrollbar, total_content_height, viewport_height);
    let is_scrolling_y = clicking_scrollbar
        .read()
        .as_ref()
        .is_some_and(|f| f.0 == Axis::Y);

    // Generate visible items with stable keys
    let visible_items = visible_range.clone().map(|i| {
        let child = (builder)(i, &builder_args);
        rsx! {
            MeasuredItem {
                key: "{i}",
                index: i,
                on_measure: on_measure,
                {child}
            }
        }
    });

    rsx!(
        rect {
            width: "{width}",
            height: "{height}",
            direction: "horizontal",
            overflow: "clip",
            reference: node_ref,
            onwheel,
            onglobalclick,
            oncaptureglobalmousemove,
            onglobalkeydown,
            onglobalkeyup,

            rect {
                width: "100%",
                height: "100%",
                padding: "{padding}",

                rect {
                    width: "1",
                    height: "{total_content_height}",
                    layer: "-1",
                }
                rect {
                    width: "100%",
                    height: "100%",
                    position: "absolute",
                    position_top: "0",
                    position_left: "0",
                    offset_y: "{content_offset + corrected_scrolled_y}",
                    {visible_items}
                }
            }

            if vertical_scrollbar_is_visible {
                ScrollBar {
                    is_vertical: true,
                    size: &applied_scrollbar_theme.size,
                    offset_y: scrollbar_y,
                    clicking_scrollbar: is_scrolling_y,
                    theme: scrollbar_theme.clone(),
                    ScrollThumb {
                        clicking_scrollbar: is_scrolling_y,
                        onmousedown: onmousedown_y,
                        width: "100%",
                        height: "{scrollbar_height}",
                        theme: scrollbar_theme.clone(),
                    }
                }
            }
        }
    )
}
