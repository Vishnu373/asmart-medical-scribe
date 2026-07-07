import { useEffect, type RefObject } from "react";

/**
 * Grow a textarea to fit its content so it never shrinks or scrolls internally —
 * the surrounding page (`main`) is the single scroll container instead. Height is
 * recomputed whenever `value` changes (typing, or streamed updates). A CSS
 * `min-height` still acts as the floor for an empty field.
 */
export function useAutoGrow(ref: RefObject<HTMLTextAreaElement | null>, value: string) {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [ref, value]);
}
