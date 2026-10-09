//! Ports must remain attributed in source packages and reachable as derived nodes.
#[test]
fn shipped_ports_have_packaged_notices_and_node_metadata() {
    let registry = wxsl_stdlib::registry();
    let mut count = 0;
    for (module, source) in wxsl_stdlib::MODULES {
        if !source.contains("// Ported from:") {
            continue;
        }
        count += 1;
        for field in [
            "SPDX-License-Identifier:",
            "Copyright",
            "Source:",
            "Revision:",
            "Symbol:",
            "Changes:",
        ] {
            assert!(source.contains(field), "{module}: missing {field}");
        }
        let revision = source
            .lines()
            .find_map(|line| line.strip_prefix("// Revision: "))
            .unwrap();
        assert_eq!(revision.len(), 40, "{module}: pin the full commit");
        assert!(revision.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(wxsl_stdlib::THIRD_PARTY_NOTICES.contains(revision));
        let relative = module.trim_start_matches("package::").replace("::", "/") + ".wxsl";
        assert!(wxsl_stdlib::THIRD_PARTY_NOTICES.contains(&relative));
        let node = wxsl_lang::node_from_source(source, module).unwrap();
        assert!(registry.get(node.id.as_str()).is_some());
        assert!(node.inputs.iter().all(|input| input.default.is_some()
            || input.splat_default.is_some()
            || input.ty.is_resource()));
    }
    assert!(count > 0, "S1 owes a worked port");
    assert!(wxsl_stdlib::THIRD_PARTY_NOTICES.contains("Permission is hereby granted"));
}
