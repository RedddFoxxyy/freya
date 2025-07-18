use dioxus::prelude::*;
use freya_core::platform::CursorIcon;
use freya_elements::{
    self as dioxus_elements,
    events::MouseEvent,
};
use freya_hooks::{
    use_animation,
    use_applied_theme,
    use_platform,
    AccordionTheme,
    AccordionThemeWith,
    AnimNum,
    Ease,
    Function,
};

/// Indicates the current status of the accordion.
#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub enum AccordionStatus {
    /// Default state.
    #[default]
    Idle,
    /// Mouse is hovering the accordion.
    Hovering,
}

/// Properties for the [`Accordion`] component.
#[derive(Props, Clone, PartialEq)]
pub struct AccordionProps {
    /// Theme override.
    pub theme: Option<AccordionThemeWith>,
    /// Inner children for the Accordion.
    pub children: Element,
    /// Summary element.
    pub summary: Element,
    /// Whether its open or not initially. Default to `false`.
    #[props(default = false)]
    pub initial_open: bool,
}

/// Show other elements under a collapsable box.
///
/// # Styling
/// Inherits the [`AccordionTheme`](freya_hooks::AccordionTheme)
#[allow(non_snake_case)]
pub fn Accordion(props: AccordionProps) -> Element {
    let theme = use_applied_theme!(&props.theme, accordion);
    let mut open = use_signal(|| props.initial_open);
    let animation = use_animation(move |_conf| {
        AnimNum::new(0., 100.)
            .time(300)
            .function(Function::Expo)
            .ease(Ease::Out)
    });
    let mut status = use_signal(AccordionStatus::default);
    let platform = use_platform();

    let animation_value = animation.get().read().read();
    let AccordionTheme {
        background,
        color,
        border_fill,
    } = theme;

    let onclick = move |_: MouseEvent| {
        open.toggle();
        if *open.read() {
            animation.start();
        } else {
            animation.reverse();
        }
    };

    use_drop(move || {
        if *status.read() == AccordionStatus::Hovering {
            platform.set_cursor(CursorIcon::default());
        }
    });

    let onmouseenter = move |_| {
        platform.set_cursor(CursorIcon::Pointer);
        status.set(AccordionStatus::Hovering);
    };

    let onmouseleave = move |_| {
        platform.set_cursor(CursorIcon::default());
        status.set(AccordionStatus::default());
    };

    rsx!(
        rect {
            onmouseenter,
            onmouseleave,
            color: "{color}",
            corner_radius: "6",
            width: "fill",
            height: "auto",
            background: "{background}",
            border: "1 inner {border_fill}",
            rect {
                width: "fill",
                overflow: "clip",
                padding: "10",
                onclick,
                {&props.summary}
            }
            rect {
                overflow: "clip",
                width: "fill",
                visible_height: "{animation_value}%",
                padding: "10",
                {&props.children}
            }
        }
    )
}

/// Properties for the [`AccordionSummary`] component.
#[derive(Props, Clone, PartialEq)]
pub struct AccordionSummaryProps {
    /// Inner children for the AccordionSummary.
    children: Element,
}

/// Intended to use as summary for an [`Accordion`].
#[allow(non_snake_case)]
pub fn AccordionSummary(props: AccordionSummaryProps) -> Element {
    rsx!({ props.children })
}

/// Properties for the [`AccordionBody`] component.
#[derive(Props, Clone, PartialEq)]
pub struct AccordionBodyProps {
    /// Inner children for the AccordionBody.
    children: Element,
}

/// Intended to wrap the body of an [`Accordion`].
#[allow(non_snake_case)]
pub fn AccordionBody(props: AccordionBodyProps) -> Element {
    rsx!(rect {
        width: "100%",
        padding: "15 0 0 0",
        {props.children}
    })
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use freya::prelude::*;
    use freya_testing::prelude::*;
    use tokio::time::sleep;

    #[tokio::test]
    pub async fn accordion() {
        fn accordion_app() -> Element {
            rsx!(
                Accordion {
                    summary: rsx!(AccordionSummary {
                        label {
                            "Accordion Summary"
                        }
                    }),
                    AccordionBody {
                        label {
                            "Accordion Body"
                        }
                    }
                }
            )
        }

        let mut utils = launch_test(accordion_app);

        let root = utils.root();
        let content = root.get(0).get(1).get(0);
        let label = content.get(0);
        utils.wait_for_update().await;

        // Accordion is closed, therefore label is hidden.
        assert!(!label.is_visible());

        // Click on the accordion
        utils.click_cursor((5., 5.)).await;

        // State somewhere in the middle
        sleep(Duration::from_millis(70)).await;
        utils.wait_for_update().await;

        // Accordion is open, therefore label is visible.
        assert!(label.is_visible());
    }
}
