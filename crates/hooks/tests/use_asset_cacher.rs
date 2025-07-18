use std::time::Duration;

use freya::prelude::*;
use freya_testing::prelude::*;
use tokio::time::sleep;

#[tokio::test]
async fn asset_cacher() {
    #[allow(non_snake_case)]
    fn Consumer() -> Element {
        let asset = use_asset(AssetConfiguration {
            age: Duration::from_millis(1).into(),
            id: "test-asset".to_string(),
        });

        let bytes = asset.try_as_bytes().unwrap().clone();

        rsx!(label { "{bytes[2]}" })
    }

    fn asset_cacher_app() -> Element {
        let mut consumers = use_signal(|| 0);
        let mut cacher = use_asset_cacher();

        use_hook(move || {
            let asset_config = AssetConfiguration {
                age: Duration::from_millis(1).into(),
                id: "test-asset".to_string(),
            };

            cacher.update_asset(asset_config, AssetBytes::Cached(vec![9, 8, 7, 6].into()));
        });

        rsx!(
            label {
                "size {cacher.size()}"
            }
            Button {
                onpress: move |_| consumers += 1 ,
                label {
                    "add"
                }
            }
            Button {
                onpress: move |_| consumers -= 1 ,
                label {
                    "remove"
                }
            }
            for i in 0..consumers() {
                Consumer {
                    key: "{i}"
                }
            }
        )
    }

    let mut utils = launch_test(asset_cacher_app);

    utils.wait_for_update().await;

    assert_eq!(utils.root().get(0).get(0).text(), Some("size 1"));

    utils.click_cursor((15., 25.)).await;

    assert_eq!(utils.root().get(0).get(0).text(), Some("size 1"));
    assert_eq!(utils.root().get(3).get(0).text(), Some("7"));

    utils.click_cursor((15., 25.)).await;

    assert_eq!(utils.root().get(0).get(0).text(), Some("size 1"));
    assert_eq!(utils.root().get(4).get(0).text(), Some("7"));

    utils.click_cursor((15., 70.)).await;

    assert_eq!(utils.root().get(0).get(0).text(), Some("size 1"));

    utils.click_cursor((15., 70.)).await;

    sleep(Duration::from_millis(5)).await;
    utils.wait_for_update().await;
    utils.wait_for_update().await;

    assert_eq!(utils.root().get(0).get(0).text(), Some("size 0"));
}
