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
            "Generated images are saved to {} as {} by default.\nIf you need to use a generated image at another path, copy it and leave the original in place unless the user explicitly asks you to delete it.\nWhen editing or generating from reference images, use images already visible in the current conversation. The `image_generation` tool has no path, base64, or image argument; reference images are supplied by prompt context. If the user provides a local image path, call `view_image` for each referenced image immediately before `image_generation`, then call `image_generation` with only the user's text prompt and edit instructions. For cutout, extraction, outfit, identity, style-transfer, or other reference-image work, phrase the prompt as an edit of the attached/local image, such as `edit the attached image to isolate only the referenced subject`, not as a fresh text-only generation. Do not claim image generation is text-only when a readable path, URL, or attached image is available.",
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
            "immediately before",
            "image_generation",
            "has no path, base64, or image argument",
            "text prompt",
            "edit the attached image",
        ] {
            assert!(
                instructions.contains(needle),
                "missing image generation instruction: {needle}"
            );
        }
    }
}
