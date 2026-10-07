//! Compile and schedule stock pipelines without a device (ADR 0048).

use wxsl_frame::pipeline::{PipelineConfig, StockPipeline, TargetConfig};
use wxsl_frame::types::TextureFormat;

fn main() {
    let config = PipelineConfig::new(TargetConfig::new(800, 600, TextureFormat::Rgba8Unorm));
    for pipeline in StockPipeline::ALL {
        let graph = pipeline.graph(&config);
        let schedule = graph.schedule().expect("the stock pipeline schedules");
        println!(
            "{pipeline}: {} passes, {} physical resources",
            graph.passes().len(),
            schedule.slots().len()
        );
        for index in schedule.order() {
            println!("  {}", graph.passes()[*index].label);
        }
    }
}
