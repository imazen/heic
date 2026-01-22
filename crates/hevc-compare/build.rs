fn main() {
    cc::Build::new()
        .cpp(true)
        .file("cpp/cabac_compare.cpp")
        .compile("cabac_compare");
}
