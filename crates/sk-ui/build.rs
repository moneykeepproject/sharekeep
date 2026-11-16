fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-light".into());
    if let Err(e) = slint_build::compile_with_config("ui/app.slint", config) {
        panic!("the Slint screens don't compile: {e}");
    }
}
