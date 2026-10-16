import { memo, useEffect } from "react";
import { createPortal } from "react-dom";
import X from "lucide-react/dist/esm/icons/x";
import type { MessageImage } from "../utils/messageRenderUtils";

export const MessageImageGrid = memo(function MessageImageGrid({
  images,
  onOpen,
  hasText,
}: {
  images: MessageImage[];
  onOpen: (index: number) => void;
  hasText: boolean;
}) {
  return (
    <div
      className={`message-image-grid${hasText ? " message-image-grid--with-text" : ""}`}
      role="list"
    >
      {images.map((image, index) => (
        <button
          key={`${image.src}-${index}`}
          type="button"
          className="message-image-thumb"
          onClick={() => onOpen(index)}
          aria-label={`Open image ${index + 1}`}
        >
          <img src={image.src} alt={image.label} loading="lazy" />
        </button>
      ))}
    </div>
  );
});

export const ImageLightbox = memo(function ImageLightbox({
  images,
  activeIndex,
  onClose,
}: {
  images: MessageImage[];
  activeIndex: number;
  onClose: () => void;
}) {
  const activeImage = images[activeIndex];

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [onClose]);

  useEffect(() => {
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = previous;
    };
  }, []);

  if (!activeImage) {
    return null;
  }

  return createPortal(
    <div
      className="message-image-lightbox"
      role="dialog"
      aria-modal="true"
      onClick={onClose}
    >
      <div
        className="message-image-lightbox-content"
        onClick={(event) => event.stopPropagation()}
      >
        <button
          type="button"
          className="message-image-lightbox-close"
          onClick={onClose}
          aria-label="Close image preview"
        >
          <X size={16} aria-hidden />
        </button>
        <img src={activeImage.src} alt={activeImage.label} />
      </div>
    </div>,
    document.body,
  );
});
