//! The person segmentation model as Burn code and weights (`$OUT_DIR/model/selfie_segmenter_landscape.{rs,bpk}`),
//! generated from the ONNX the CPU path runs, for the GPU path (`src/video/effects/gpu.rs`). One model file, two
//! runtimes: the generated code is never edited or committed.

const MODEL: &str = "assets/segmentation/selfie_segmenter_landscape.onnx";

fn main() {
    println!("cargo::rerun-if-changed={MODEL}");
    burn_onnx::ModelGen::new()
        .input(MODEL)
        .out_dir("model/")
        .run_from_script();
}
