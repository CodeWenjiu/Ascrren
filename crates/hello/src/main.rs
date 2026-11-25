//! `hello` crate：通过 `crate_macro` 声明模块的最小可运行示例。

crate_macro::mod_pub!(greeting);

fn main() {
    crate::greeting::hello();
}
