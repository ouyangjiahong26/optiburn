// 遮罩 + 卡片 + 取消与确认两键；不给 cancelText 时是单键提示框，
// 遮罩点击与 Escape 都走默认的收尾动作。
import { useEffect } from "react";

export function ConfirmDialog({
  open,
  title,
  message,
  confirmText,
  cancelText,
  tone,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  title: string;
  message: string;
  confirmText: string;
  cancelText?: string;
  tone?: "danger";
  onConfirm: () => void;
  onCancel: () => void;
}) {
  useEffect(() => {
    if (!open) {
      return;
    }
    const dismiss = cancelText === undefined ? onConfirm : onCancel;
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        dismiss();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, cancelText, onConfirm, onCancel]);

  if (!open) {
    return null;
  }
  const dismiss = cancelText === undefined ? onConfirm : onCancel;
  return (
    <div className="dialog-overlay" onClick={dismiss}>
      <div
        className="dialog-card"
        role="dialog"
        aria-modal="true"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="dialog-title">{title}</h2>
        <div className="dialog-message">{message}</div>
        <div className="dialog-actions">
          {cancelText !== undefined && (
            <button type="button" className="btn" onClick={onCancel}>
              {cancelText}
            </button>
          )}
          <button
            type="button"
            className={tone === "danger" ? "btn btn-danger" : "btn btn-primary"}
            onClick={onConfirm}
            autoFocus
          >
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
