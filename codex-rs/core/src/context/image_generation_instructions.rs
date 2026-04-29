use super::ContextualUserFragment;
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageGenerationInstructions {
    image_output_dir: String,
    image_output_path: String,
}

impl ImageGenerationInstructions {
    pub(crate) fn new(image_output_dir: impl Display, image_output_path: impl Display) -> Self {
        Self {
            image_output_dir: image_output_dir.to_string(),
            image_output_path: image_output_path.to_string(),
        }
    }
}

impl ContextualUserFragment for ImageGenerationInstructions {
    const ROLE: &'static str = "developer";
    const START_MARKER: &'static str = "";
    const END_MARKER: &'static str = "";

    fn body(&self) -> String {
        format!(
            "Generated images are saved to {} as {} by default.\nIf you need to use a generated image at another path, copy it and leave the original in place unless the user explicitly asks you to delete it.\nWhen editing or generating from reference images, use images already in conversation. If the user provides a local image path, call `view_image` for each referenced image before calling `image_generation` with the user's text prompt.",
            self.image_output_dir, self.image_output_path
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_explain_reference_image_flow() {
        let instructions = ImageGenerationInstructions::new("/tmp", "/tmp/image.png").body();
        for needle in [
            "Generated images are saved to /tmp as /tmp/image.png by default.",
            "reference images",
            "local image path",
            "view_image",
            "image_generation",
            "text prompt",
        ] {
            assert!(
                instructions.contains(needle),
                "missing image generation instruction: {needle}"
            );
        }
    }
}
