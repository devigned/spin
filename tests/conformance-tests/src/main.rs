fn main() {
    let spin_binary: std::path::PathBuf = std::env::args()
        .nth(1)
        .expect("expected first argument to be path to spin binary")
        .into();
    conformance_tests::run_tests("canary", move |test| {
        conformance::run_test(test, &spin_binary)
    })
    .unwrap();
}
