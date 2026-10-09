#[test]
fn c_header_is_generated_from_the_current_layout_table() {
    assert_eq!(wxsl_frame::HOST_HEADER, include_str!("../../../dawn/include/wxsl_host.h"),
        "regenerate with cargo run -p wxsl-frame --example export_host_header -- dawn/include/wxsl_host.h");
}
