pub mod sink;

pub use sink::*;

mod gst_plugin {
    fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
        // Register the sink element
        super::sink::register(plugin)?;

        Ok(())
    }

    gst::plugin_define!(
        gst_rerunsink,
        env!("CARGO_PKG_DESCRIPTION"),
        plugin_init,
        env!("CARGO_PKG_VERSION"),
        "LGPL",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_REPOSITORY"),
        env!("BUILD_REL_DATE")
    );
}

// Re-export the plugin registration function
pub use gst_plugin::plugin_register_static;
