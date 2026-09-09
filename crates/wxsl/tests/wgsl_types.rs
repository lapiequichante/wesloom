//! `ValueType`'s operator typing rules, checked against wgpu's validator.
//!
//! `wxsl_core::node::TypeRule` decides which pairs of socket types a
//! `math.add`/`math.multiply`-shaped node will let a graph combine, and
//! therefore which WGSL the generator is allowed to emit. Getting that table
//! wrong produces a graph that validates and a shader the driver rejects —
//! the one failure mode no amount of graph-level testing can catch, because
//! the authority on it is the shader compiler, not us.
//!
//! So this asks the authority: for every pair of types, does wgpu accept
//! `x op y`, and does `TypeRule` agree? Like `render_cube.rs`, it **skips**
//! (prints a note and passes) when no adapter is available, so do not read a
//! pass as proof that it ran.

use wxsl::core::node::{TypeRule, ValueType};
use wxsl::render::gpu::GpuContext;

/// A device, or `None` when the machine has no adapter.
fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(error) => {
            println!("skipping: no wgpu adapter ({error})");
            None
        }
    }
}

/// Whether wgpu accepts a module containing `body` as a function body.
fn accepts(gpu: &GpuContext, a: ValueType, b: ValueType, expr: &str) -> bool {
    let source = format!(
        "fn probe(x: {}, y: {}) {{ let z = {expr}; }}\n\
         @compute @workgroup_size(1) fn main() {{}}\n",
        a.wxsl_type(),
        b.wxsl_type(),
    );
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    pollster::block_on(scope.pop()).is_none()
}

/// Compare `rule` against wgpu for every ordered pair drawn from `types`.
fn agrees(gpu: &GpuContext, rule: TypeRule, operator: &str, types: &[ValueType]) {
    let mut disagreements = Vec::new();
    for &a in types {
        for &b in types {
            let wgpu_accepts = accepts(gpu, a, b, &format!("x {operator} y"));
            let rule_accepts = rule.apply(a, b).is_some();
            if wgpu_accepts != rule_accepts {
                disagreements.push(format!(
                    "{a} {operator} {b}: wgpu {}, {rule} rule {}",
                    if wgpu_accepts { "accepts" } else { "rejects" },
                    if rule_accepts { "accepts" } else { "rejects" },
                ));
            }
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} disagreement(s) on `{operator}`:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}

#[test]
fn componentwise_is_what_wgpu_accepts_for_the_operators_that_use_it() {
    let Some(gpu) = gpu() else { return };
    // `+` and `-` take matrices; `/` and `%` take none, which is why
    // `wxsl_stdlib::registry`'s `Binary::matrices` exists rather than the
    // rule itself distinguishing them.
    for operator in ["+", "-"] {
        agrees(
            &gpu,
            TypeRule::Componentwise,
            operator,
            &ValueType::operands(),
        );
    }
    for operator in ["/", "%"] {
        agrees(&gpu, TypeRule::Componentwise, operator, ValueType::FLOATS);
    }
}

#[test]
fn product_is_what_wgpu_accepts_for_multiplication() {
    let Some(gpu) = gpu() else { return };
    agrees(&gpu, TypeRule::Product, "*", &ValueType::operands());
}

#[test]
fn the_builtin_backed_families_really_do_need_matching_operands() {
    // `math.power`, `math.minimum`, `math.maximum` and `math.arctangent2`
    // share one type parameter across both operands rather than combining
    // two, because the WGSL builtins behind them have a single `(T, T) -> T`
    // overload. This is that claim, checked.
    let Some(gpu) = gpu() else { return };
    for builtin in ["pow", "min", "max", "atan2"] {
        for &a in ValueType::FLOATS {
            for &b in ValueType::FLOATS {
                let accepted = accepts(&gpu, a, b, &format!("{builtin}(x, y)"));
                assert_eq!(
                    accepted,
                    a == b,
                    "`{builtin}({a}, {b})` is {}, expected {}",
                    if accepted { "accepted" } else { "rejected" },
                    if a == b { "accepted" } else { "rejected" },
                );
            }
        }
    }
}

#[test]
fn the_unary_families_take_no_matrix() {
    // `math.negate`, `math.absolute`, `math.saturate` and the rest of the
    // unary table are generic over `ValueType::FLOATS` and not over
    // `ValueType::operands()`, even though `+`, `-` and `*` do take
    // matrices. `-mat3x3f` is simply not WGSL, and nor is any of the
    // component-wise builtins applied to one.
    let Some(gpu) = gpu() else { return };
    for expr in ["-x", "abs(x)", "saturate(x)", "sin(x)", "fract(x)"] {
        for &ty in ValueType::FLOATS {
            assert!(
                accepts(&gpu, ty, ty, expr),
                "`{expr}` should be accepted at {ty}"
            );
        }
        for &ty in ValueType::MATRICES {
            assert!(
                !accepts(&gpu, ty, ty, expr),
                "`{expr}` should be rejected at {ty}"
            );
        }
    }
}
