//! Zennode decode node definition for heic.
//!
//! Provides [`DecodeHeic`], a self-documenting pipeline node that bridges
//! zennode's parameter system with [`crate::HeicDecoderConfig`].
//!
//! Feature-gated behind `feature = "zennode"`.

use zennode::*;

use crate::codec::HeicDecoderConfig;

/// HEIC/HEIF decode node for zennode pipelines.
///
/// Controls HEIC decoding behavior, including extraction of supplemental
/// images (gain maps, depth maps, mattes) and thumbnail decoding.
///
/// **RIAPI**: `?heic.gain_map=true&heic.thumbnail=false`
/// **JSON**: `{ "extract_gain_map": true, "decode_thumbnail": false }`
///
/// Convert to [`HeicDecoderConfig`] via
/// [`to_decoder_config()`](DecodeHeic::to_decoder_config).
///
/// # Wired flags
///
/// - **`extract_gain_map`**: Mapped to [`HeicDecoderConfig::extract_gain_map`].
///   Controls whether the HDR gain map auxiliary image is decoded (opt-in,
///   default `true` in the node, `false` in the raw config).
/// - **`extract_depth`**: Mapped to [`HeicDecoderConfig::extract_depth`].
///   Controls whether the depth map auxiliary image is decoded (default `false`).
///
/// # Gaps
///
/// - **`extract_mattes`**: Matte auxiliary image decoding is not yet wired
///   through the zencodec layer. This field is a placeholder.
/// - **`decode_thumbnail`**: Thumbnail decoding is not yet exposed through
///   the zencodec wrapper. The HEIF container stores thumbnails, but the
///   current decode path always decodes the primary item.
#[derive(Node, Clone, Debug)]
#[node(id = "heic.decode", group = Decode, role = Decode)]
#[node(tags("heic", "heif", "hdr", "depth"))]
pub struct DecodeHeic {
    /// Whether to extract the HDR gain map auxiliary image, if present.
    ///
    /// When enabled (default), the gain map will be decoded and attached
    /// to the output extensions as a separate image. Apple ProRAW and
    /// iPhone HDR photos typically include gain maps.
    ///
    /// Mapped to [`HeicDecoderConfig::extract_gain_map`].
    #[param(default = true)]
    #[param(section = "Supplements", label = "Extract Gain Map")]
    #[kv("heic.gain_map")]
    pub extract_gain_map: bool,

    /// Whether to extract the depth map auxiliary image, if present.
    ///
    /// Portrait-mode photos from iPhones include depth maps for
    /// computational photography effects.
    ///
    /// Mapped to [`HeicDecoderConfig::extract_depth`].
    #[param(default = false)]
    #[param(section = "Supplements", label = "Extract Depth Map")]
    #[kv("heic.depth")]
    pub extract_depth: bool,

    /// Whether to extract segmentation mattes, if present.
    ///
    /// Mattes (hair, skin, teeth) are used for portrait lighting and
    /// other segmentation-based effects in Apple's camera pipeline.
    ///
    /// **Note:** Not yet wired through the zencodec layer. This field
    /// is a placeholder for when matte extraction support lands.
    #[param(default = false)]
    #[param(section = "Supplements", label = "Extract Mattes")]
    #[kv("heic.mattes")]
    pub extract_mattes: bool,

    /// Whether to decode the embedded thumbnail instead of the primary image.
    ///
    /// HEIF containers often include a small thumbnail for fast previews.
    /// When enabled, the decoder will return the thumbnail rather than
    /// the full-resolution image.
    ///
    /// **Note:** Thumbnail decoding is not yet exposed through the
    /// zencodec wrapper. This field is a placeholder.
    #[param(default = false)]
    #[param(section = "Main", label = "Decode Thumbnail")]
    #[kv("heic.thumbnail")]
    pub decode_thumbnail: bool,
}

impl Default for DecodeHeic {
    fn default() -> Self {
        Self {
            extract_gain_map: true,
            extract_depth: false,
            extract_mattes: false,
            decode_thumbnail: false,
        }
    }
}

impl DecodeHeic {
    /// Convert this node into a [`HeicDecoderConfig`].
    ///
    /// Maps the supplement extraction flags (`extract_gain_map`,
    /// `extract_depth`) through to [`HeicDecoderConfig`]. These control
    /// whether the decoder performs the (expensive) pixel decode of
    /// auxiliary images — container metadata is always populated cheaply.
    ///
    /// Fields not yet wired to the decoder:
    /// - **`extract_mattes`**: Matte auxiliary image decode is not yet
    ///   supported in the zencodec adapter.
    /// - **`decode_thumbnail`**: Thumbnail decode path is not yet exposed.
    ///
    /// The pipeline layer should inspect these fields directly (via
    /// [`DecodeHeic`]) for behaviors not yet supported by the config.
    #[must_use]
    pub fn to_decoder_config(&self) -> HeicDecoderConfig {
        HeicDecoderConfig::new()
            .with_extract_gain_map(self.extract_gain_map)
            .with_extract_depth(self.extract_depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_basics() {
        let schema = DECODE_HEIC_NODE.schema();
        assert_eq!(schema.id, "heic.decode");
        assert_eq!(schema.group, NodeGroup::Decode);
        assert_eq!(schema.role, NodeRole::Decode);
        assert!(schema.tags.contains(&"heic"));
        assert!(schema.tags.contains(&"heif"));
        assert!(schema.tags.contains(&"hdr"));
        assert!(schema.tags.contains(&"depth"));

        let param_names: alloc::vec::Vec<&str> = schema.params.iter().map(|p| p.name).collect();
        assert!(param_names.contains(&"extract_gain_map"));
        assert!(param_names.contains(&"extract_depth"));
        assert!(param_names.contains(&"extract_mattes"));
        assert!(param_names.contains(&"decode_thumbnail"));
    }

    #[test]
    fn default_values() {
        let node = DECODE_HEIC_NODE.create_default().unwrap();
        assert_eq!(
            node.get_param("extract_gain_map"),
            Some(ParamValue::Bool(true))
        );
        assert_eq!(
            node.get_param("extract_depth"),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(
            node.get_param("extract_mattes"),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(
            node.get_param("decode_thumbnail"),
            Some(ParamValue::Bool(false))
        );
    }

    #[test]
    fn kv_keys_coverage() {
        let schema = DECODE_HEIC_NODE.schema();

        let gain_map = schema
            .params
            .iter()
            .find(|p| p.name == "extract_gain_map")
            .unwrap();
        assert_eq!(gain_map.kv_keys, &["heic.gain_map"]);

        let depth = schema
            .params
            .iter()
            .find(|p| p.name == "extract_depth")
            .unwrap();
        assert_eq!(depth.kv_keys, &["heic.depth"]);

        let mattes = schema
            .params
            .iter()
            .find(|p| p.name == "extract_mattes")
            .unwrap();
        assert_eq!(mattes.kv_keys, &["heic.mattes"]);

        let thumbnail = schema
            .params
            .iter()
            .find(|p| p.name == "decode_thumbnail")
            .unwrap();
        assert_eq!(thumbnail.kv_keys, &["heic.thumbnail"]);
    }

    #[test]
    fn kv_parsing() {
        let mut kv = KvPairs::from_querystring("heic.gain_map=false&heic.depth=true");
        let node = DECODE_HEIC_NODE.from_kv(&mut kv).unwrap().unwrap();
        assert_eq!(
            node.get_param("extract_gain_map"),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(
            node.get_param("extract_depth"),
            Some(ParamValue::Bool(true))
        );
        // Unspecified params keep defaults
        assert_eq!(
            node.get_param("extract_mattes"),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(
            node.get_param("decode_thumbnail"),
            Some(ParamValue::Bool(false))
        );
        assert_eq!(kv.unconsumed().count(), 0);
    }

    #[test]
    fn kv_parsing_no_match() {
        let mut kv = KvPairs::from_querystring("w=800&h=600");
        let result = DECODE_HEIC_NODE.from_kv(&mut kv).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn to_decoder_config_maps_flags() {
        let node = DecodeHeic::default();
        let config = node.to_decoder_config();
        // Default node has extract_gain_map=true, extract_depth=false
        assert!(config.extract_gain_map);
        assert!(!config.extract_depth);

        let node2 = DecodeHeic {
            extract_gain_map: false,
            extract_depth: true,
            ..DecodeHeic::default()
        };
        let config2 = node2.to_decoder_config();
        assert!(!config2.extract_gain_map);
        assert!(config2.extract_depth);
    }

    #[test]
    fn downcast() {
        let node = DECODE_HEIC_NODE.create_default().unwrap();
        let decode = node.as_any().downcast_ref::<DecodeHeic>().unwrap();
        assert!(decode.extract_gain_map);
        assert!(!decode.extract_depth);
        assert!(!decode.extract_mattes);
        assert!(!decode.decode_thumbnail);
    }
}
