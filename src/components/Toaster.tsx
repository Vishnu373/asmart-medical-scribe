import { useEffect } from "react";
import { useAppStore, type Toast } from "@/state";

const AUTO_DISMISS_MS = 6000;

function ToastItem({ toast }: { toast: Toast }) {
  const dismissToast = useAppStore((s) => s.dismissToast);

  useEffect(() => {
    const t = setTimeout(() => dismissToast(toast.id), AUTO_DISMISS_MS);
    return () => clearTimeout(t);
  }, [toast.id, dismissToast]);

  return (
    <div
      role="alert"
      className={`flex items-start gap-3 rounded-md border px-3 py-2 text-sm shadow-lg ${
        toast.kind === "error"
          ? "border-red-800 bg-red-950 text-red-200"
          : "border-neutral-700 bg-neutral-900 text-neutral-200"
      }`}
    >
      <span className="flex-1">
        {toast.message}
        {toast.count > 1 && (
          <span className="ml-2 rounded bg-black/30 px-1.5 text-xs">×{toast.count}</span>
        )}
      </span>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={() => dismissToast(toast.id)}
        className="text-neutral-400 hover:text-neutral-100"
      >
        ✕
      </button>
    </div>
  );
}

/** Stacked transient notifications (errors + info), bottom-right. */
export default function Toaster() {
  const toasts = useAppStore((s) => s.toasts);
  if (toasts.length === 0) return null;

  return (
    <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex w-80 flex-col gap-2">
      {toasts.map((t) => (
        <div key={t.id} className="pointer-events-auto">
          <ToastItem toast={t} />
        </div>
      ))}
    </div>
  );
}
