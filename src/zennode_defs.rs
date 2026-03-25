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
/// # Gaps
///
/// The underlying [`HeicDecoderConfig`] currently has no configuration knobs —
/// the native `DecoderConfig` is a zero-field struct. These node fields
/// document *intent* for the pipeline layer:
///
/// - **`extract_gain_map`**: Gain map extraction is currently unconditional
///   in the zencodec adapter (always extracted when present). This field
///   will gate extraction once the adapter respects it.
/// - **`extract_depth`** / **`extract_mattes`**: Depth and matte auxiliary
///   image decoding is not yet wired through the zencodec layer. These
///   fields are placeholders for when that support lands.
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
    /// **Note:** The zencodec adapter currently extracts gain maps
    /// unconditionally. This field will gate that behavior once the
    /// adapter is updated.
    #[param(default = true)]
    #[param(section = "Supplements", label = "Extract Gain Map")]
    #[kv("heic.gain_map")]
    pub extract_gain_map: bool,

    /// Whether to extract the depth map auxiliary image, if present.
    ///
    /// Portrait-mode photos from iPhones include depth maps for
    /// computational photography effects.
    ///
    /// **Note:** Depth map extraction is not yet wired through the
    /// zencodec layer. This field is a placeholder.
    #[param(default = false)]
    #[param(section = "Supplements", label = "Extract Depth Map")]
    #[kv("heic.depth")]
    pub extract_depth: bool,

    /// Whether to extract segmentation mattes, if present.
    ///
    /// Mattes (hair, skin, teeth) are used for portrait lighting and
    /// other segmentation-based effects in Apple's camera pipeline.
    ///
    /// **Note:** Matte extraction is not yet wired through the
    /// zencodec layer. This field is a placeholder.
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
    /// Currently returns a default config because [`HeicDecoderConfig`] has
    /// no tunable parameters — the native decoder is a zero-config struct.
    ///
    /// The node fields (`extract_gain_map`, `extract_depth`, `extract_mattes`,
    /// `decode_thumbnail`) capture pipeline intent but are not yet plumbed
    /// through to the decoder. The pipeline layer should inspect these fields
    /// directly (via [`DecodeHeic`]) to decide post-decode behavior:
    ///
    /// - Gate gain map attachment on `extract_gain_map`
    /// - Trigger auxiliary image decode for `extract_depth` / `extract_mattes`
    /// - Route to thumbnail decode path for `decode_thumbnail`
    #[must_use]
    pub fn to_decoder_config(&self) -> HeicDecoderConfig {
        // HeicDecoderConfig wraps the native DecoderConfig which has no
        // configuration knobs. All node fields must be interpreted by the
        // pipeline layer rather than the decoder itself.
        HeicDecoderConfig::new()
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
    fn to_decoder_config_returns_default() {
        let node = DecodeHeic::default();
        let _config = node.to_decoder_config();
        // HeicDecoderConfig has no observable state to assert on —
        // just verify it constructs without panicking.
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
