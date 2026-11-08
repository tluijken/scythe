//! Loads a VST 2 plugin, prints what it declares and runs one block of noise through it.
//!
//! cargo run -p scythe-engine --example vst2_probe -- /path/to/plugin.so
use scythe_engine::vst2::Vst2Plugin;

fn main() {
    for path in std::env::args().skip(1) {
        match Vst2Plugin::load(path.as_ref(), 48000.0, 512) {
            Ok(mut p) => {
                let (name, vendor) = (p.name(), p.vendor());
                println!(
                    "{name} by {vendor}: id {} in {} out {} params {} flags {:#x} chunk {} bytes",
                    p.unique_id(),
                    p.num_inputs(),
                    p.num_outputs(),
                    p.num_params(),
                    p.flags(),
                    p.chunk().len()
                );
                for i in 0..p.num_params().min(4) {
                    println!("  param {i}: {} = {} ({})", p.param_name(i), p.param(i), p.param_display(i));
                }
                p.resume();
                let ins: Vec<Vec<f32>> = (0..p.num_inputs())
                    .map(|c| (0..512).map(|i| ((i * 7 + c) % 13) as f32 / 13.0 - 0.5).collect())
                    .collect();
                let mut outs: Vec<Vec<f32>> = (0..p.num_outputs()).map(|_| vec![0.0; 512]).collect();
                let in_ptrs: Vec<*const f32> = ins.iter().map(|b| b.as_ptr()).collect();
                let mut out_ptrs: Vec<*mut f32> = outs.iter_mut().map(|b| b.as_mut_ptr()).collect();
                p.process(&in_ptrs, &mut out_ptrs, 512);
                let rms = |b: &[f32]| (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).sqrt();
                println!(
                    "  in rms {:.4} out rms {:.4}",
                    ins.first().map_or(0.0, |b| rms(b)),
                    outs.first().map_or(0.0, |b| rms(b))
                );
            }
            Err(e) => println!("{e}"),
        }
    }
}
