with open("saturn-core/src/lib.rs", "r") as f:
    text = f.read()

allows = """#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::assertions_on_constants,
    clippy::erasing_op,
    clippy::eq_op,
    clippy::manual_clamp,
    clippy::useless_vec,
    clippy::bool_assert_comparison,
    clippy::identity_op
)]
"""

if "#![allow(" not in text:
    text = allows + text

with open("saturn-core/src/lib.rs", "w") as f:
    f.write(text)

