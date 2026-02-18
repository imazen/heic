fn main() {
    let data = std::fs::read("/home/lilith/work/heic/libheif/examples/example.heic")
        .expect("Failed to read test file");
    let decoder = heic_decoder::HeicDecoder::new();
    let _result = decoder.decode(&data);
}
