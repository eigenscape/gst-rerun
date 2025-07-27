pub mod pipeline_graph;
mod tracer;

pub use tracer::*;

// Note: This function is also defined by the plugin_define! macro bellow,
// however it can't be called before gst::init, which is necessary
// for properly registering the tracer.
pub fn plugin_register_static() -> Result<(), glib::BoolError> {
    gst::Tracer::register(
        None, // No plugin object needed for testing
        "reruntracing",
        <RerunTracer as glib::types::StaticType>::static_type(),
    )?;
    Ok(())
}

mod gst_plugin {

    fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
        // Register the tracer
        gst::Tracer::register(
            Some(plugin),
            "reruntracing",
            <super::RerunTracer as glib::types::StaticType>::static_type(),
        )?;

        Ok(())
    }

    gst::plugin_define!(
        gst_reruntracer,
        env!("CARGO_PKG_DESCRIPTION"),
        plugin_init,
        env!("CARGO_PKG_VERSION"),
        "GPL",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_REPOSITORY"),
        env!("BUILD_REL_DATE")
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    pub(crate) fn init() {
        INIT.call_once(|| {
            plugin_register_static().expect("Failed to register tracer plugin");
            std::env::set_var(
                "GST_TRACERS",
                "reruntracing(debounce=100,spawn-viewer=false)",
            );
            gst::init().expect("Failed to initialize GStreamer");
        });
    }
}
