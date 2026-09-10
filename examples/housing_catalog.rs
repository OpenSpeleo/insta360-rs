//! Regenerate the checked-in source table: cargo run --example housing_catalog.
use insta360_rs::profile::{camera_profiles, housing_references, physical_curves};
use std::fmt::Write;

fn catalog() -> String {
    let mut result = String::from("# Camera and housing source catalog\n\nGenerated from `src-rust/profile.rs`, `src-rust/profile/curves.rs` and\n`src-rust/profile/housings.rs` by `cargo run --locked --example housing_catalog`.\nSee [housing behavior and limitations](housings.md). Every path below is relative\nto its named software distribution. Registered offsets still require supported\nprojection, input layout and source metadata; recognition is not a claim of\nreal-camera qualification.\n\n## Registered optics\n\n| Camera | Lens ID | Housing | Environment | Lens accessory | Full FOV / blend (degrees) | Housing contour | Source |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for camera in camera_profiles() {
        for lens in camera.lenses {
            let mask = lens
                .mask_recipe
                .map(|recipe| {
                    let points = recipe
                        .lower_hemisphere_boundary
                        .iter()
                        .map(|point| {
                            format!("({},{})", point.azimuth_degrees, point.half_fov_degrees)
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!(
                        "{points}; {:?}; feather {}",
                        recipe.interpolation, recipe.feather_weight_per_pixel
                    )
                })
                .unwrap_or_else(|| "No additional housing contour registered".into());
            let blend = lens
                .fallback
                .blend_angle_degrees
                .map(|angle| angle.to_string())
                .unwrap_or_else(|| "Unestablished".into());
            let provenance = lens.lens_id_provenance;
            let mask_source = lens
                .mask_recipe
                .map(|recipe| {
                    format!(
                        "; mask `{}`: `{}`",
                        recipe.provenance.source_path, recipe.provenance.evidence
                    )
                })
                .unwrap_or_default();
            writeln!(result,"| {} | {} | {:?} | {:?} | {:?} | {} / {} | {} | {} {}: `{}`; `{}`; fallback `{}`: `{}`{} |",camera.canonical_name,lens.lens_id,lens.selection.housing,lens.selection.environment,lens.selection.lens_accessory,lens.fallback.full_fov_degrees,blend,mask,provenance.software(),provenance.version(),provenance.source_path,provenance.evidence,lens.fallback_provenance.source_path,lens.fallback_provenance.evidence,mask_source).unwrap();
        }
    }
    result.push_str("\nRows identify their source software and artifact path. The iOS SDK 1.10.4 arm64\n`INSCoreMedia` binary has SHA-256\n`3b905b46e46053d9c426c4af8bb28e449666d3ddf1c03419ab564ed2b0a01409`.\nThe Android SDK 2.1.5 arm64 `libarvbmg.so` APK member has SHA-256\n`6cea9beda80ffe53eea85f04503a07cd25ff7a573b466df6d8e54aec7575a7e0`.\nThe X5 standard and Pro contours are independently corroborated by Studio 5.9.10\n`Contents/MacOS/Insta360 Studio` arm64, SHA-256\n`105af1515ebc6d0fa0c64786fd30474fa4a0803e788f29225f2c3ce372718155`.\n\n## Additional official housing references\n\n| Camera | Housing | Environment | Lens IDs | Source | Evidence | Limitation |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for row in housing_references() {
        let ids = if row.lens_ids.is_empty() {
            "Unestablished".into()
        } else {
            row.lens_ids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        writeln!(
            result,
            "| {} | {:?} | {:?} | {} | {}: `{}` | {} | {} |",
            row.camera,
            row.housing,
            row.environment,
            ids,
            row.software,
            row.path,
            row.evidence,
            row.limitation
        )
        .unwrap();
    }
    result.push_str("\n## Recovered generic physical curves\n\nThese are degree-to-physical-radius polynomials `c0 + c1*t + ... + c4*t^4`.\nThey are distinct from recorded six-coefficient transforms and from per-unit\nprojection intrinsics. A curve alone does not enable conversion or rendering.\nEach row identifies its exact iOS or Android source binary; hashes are above.\n\n| Lens ID | Coefficients | Source | Native evidence |\n| --- | --- | --- | --- |\n");
    for curve in physical_curves() {
        writeln!(
            result,
            "| {} | `{:?}` | {} {}: `{}` | `{}` |",
            curve.lens_id,
            curve.coefficients,
            curve.provenance.software(),
            curve.provenance.version(),
            curve.provenance.source_path,
            curve.provenance.evidence
        )
        .unwrap();
    }
    result
}

fn main() {
    print!("{}", catalog());
}

#[test]
fn documented_catalog_matches_code() {
    assert!(
        include_str!("../docs/housing-catalog.md") == catalog(),
        "Regenerate docs/housing-catalog.md with cargo run --locked --example housing_catalog"
    );
}
