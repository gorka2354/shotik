mod common;

use common::display;
use serde_json::json;
use shotik_core::{Display, Rect};
use shotik_mcp::region_map::{describe_displays, map_global_region, validate_region};

fn displays() -> Vec<Display> {
    vec![
        display(
            1_836_973_850,
            Rect::new(0, 0, 2560, 1440),
            1.0,
            Rect::default(),
            true,
        ),
        display(
            1_609_006_897,
            Rect::new(2560, 261, 1920, 1080),
            1.0,
            Rect::default(),
            false,
        ),
    ]
}

// Every vector from test/unit/region-map.test.js, including its reported layout.
#[test]
fn secondary_monitor_maps_to_relative_coords() {
    let m = map_global_region(&displays(), Rect::new(2560, 261, 1900, 1060).into(), None).unwrap();
    assert_eq!(m.display_id, 1_609_006_897);
    assert_eq!(m.rel_rect, Rect::new(0, 0, 1900, 1060));
    assert!(!m.clipped);
}

#[test]
fn primary_keeps_global_equal_to_relative() {
    let m = map_global_region(&displays(), Rect::new(100, 200, 300, 150).into(), None).unwrap();
    assert_eq!(m.display_id, 1_836_973_850);
    assert_eq!(m.rel_rect, Rect::new(100, 200, 300, 150));
}

#[test]
fn negative_origin_displays_work() {
    let ds = vec![display(
        7,
        Rect::new(-1920, -120, 1920, 1080),
        1.0,
        Rect::default(),
        true,
    )];
    let m = map_global_region(&ds, Rect::new(-1900, -100, 400, 300).into(), None).unwrap();
    assert_eq!(m.display_id, 7);
    assert_eq!(m.rel_rect, Rect::new(20, 20, 400, 300));
}

#[test]
fn spanning_monitors_clips_to_largest_share() {
    let m = map_global_region(&displays(), Rect::new(2260, 400, 1000, 200).into(), None).unwrap();
    assert_eq!(m.display_id, 1_609_006_897);
    assert_eq!(m.global_rect, Rect::new(2560, 400, 700, 200));
    assert!(m.clipped);
}

#[test]
fn hanging_off_bottom_edge_is_clipped() {
    let m = map_global_region(&displays(), Rect::new(100, 1400, 200, 200).into(), None).unwrap();
    assert_eq!(m.global_rect, Rect::new(100, 1400, 200, 40));
    assert!(m.clipped);
}

#[test]
fn outside_displays_has_exact_error_with_display_list() {
    let ds = displays();
    let error = map_global_region(&ds, Rect::new(9000, 9000, 100, 100).into(), None)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Region 100x100 at (9000, 9000) does not intersect any display. Displays: id 1836973850: 2560x1440 at (0, 0); id 1609006897: 1920x1080 at (2560, 261)");
}

#[test]
fn display_id_restricts_mapping() {
    let m = map_global_region(
        &displays(),
        Rect::new(2260, 400, 1000, 200).into(),
        Some(1_836_973_850.0),
    )
    .unwrap();
    assert_eq!(m.display_id, 1_836_973_850);
    assert_eq!(m.global_rect, Rect::new(2260, 400, 300, 200));
}

#[test]
fn unknown_display_id_has_exact_error() {
    let error = map_global_region(&displays(), Rect::new(0, 0, 10, 10).into(), Some(42.0))
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        format!(
            "Unknown display_id 42. Displays: {}",
            describe_displays(&displays())
        )
    );
}

#[test]
fn missing_fields_are_rejected() {
    let error = validate_region(&json!({"region":{"x":1,"y":2,"width":3,"height":4}}))
        .unwrap_err()
        .to_string();
    assert_eq!(error, "Invalid region: \"x\" must be a finite number, got undefined. Pass x, y, width, height in global virtual-desktop DIP coordinates (same space as list_displays bounds).");
}

#[test]
fn numeric_strings_are_accepted() {
    assert_eq!(
        validate_region(&json!({"x":"2560","y":"261","width":"100","height":"50.4"})).unwrap(),
        Rect::new(2560, 261, 100, 50).into()
    );
}

#[test]
fn zero_and_negative_sizes_are_rejected() {
    for width in [0, -2] {
        assert_eq!(
            validate_region(&json!({"x":0,"y":0,"width":width,"height":10}))
                .unwrap_err()
                .to_string(),
            format!("Invalid region: width and height must be >= 1, got {width}x10")
        );
    }
}

#[test]
fn non_numeric_garbage_is_rejected() {
    assert_eq!(validate_region(&json!({"x":"left","y":0,"width":10,"height":10})).unwrap_err().to_string(),
        "Invalid region: \"x\" must be a finite number, got \"left\". Pass x, y, width, height in global virtual-desktop DIP coordinates (same space as list_displays bounds).");
}

#[test]
fn js_rounding_coercion_and_each_invalid_field() {
    assert_eq!(
        validate_region(&json!({"x":-1.5,"y":null,"width":true,"height":"0x10"})).unwrap(),
        Rect::new(-1, 0, 1, 16).into()
    );
    for key in ["x", "y", "width", "height"] {
        for value in [json!("Infinity"), json!({}), json!([1, 2])] {
            let mut args = json!({"x":0,"y":0,"width":10,"height":10});
            args[key] = value.clone();
            assert_eq!(validate_region(&args).unwrap_err().to_string(), format!("Invalid region: \"{key}\" must be a finite number, got {value}. Pass x, y, width, height in global virtual-desktop DIP coordinates (same space as list_displays bounds)."));
        }
    }
}

#[test]
fn ties_use_first_display_and_edges_are_not_intersections() {
    let ds = displays();
    let m = map_global_region(&ds, Rect::new(2460, 400, 200, 200).into(), None).unwrap();
    assert_eq!(m.display_id, ds[0].id);
    assert!(map_global_region(&ds, Rect::new(4480, 400, 1, 1).into(), None).is_err());
    assert_eq!(map_global_region(&ds, Rect::new(3000,400,10,10).into(), Some(ds[0].id as f64)).unwrap_err().to_string(),
        "Region 10x10 at (3000, 400) does not intersect display 1836973850. Displays: id 1836973850: 2560x1440 at (0, 0)");
}

#[test]
fn finite_large_regions_are_clipped_without_integer_overflow() {
    let rect = validate_region(&json!({"x":-1e12,"y":-1e12,"width":2e12,"height":2e12})).unwrap();
    let m = map_global_region(&displays(), rect, None).unwrap();
    assert_eq!(m.global_rect, displays()[0].bounds);
}
