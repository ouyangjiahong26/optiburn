// 标签 + 控件 + 可选说明文字的表单行。
import type { ReactNode } from "react";

export function FormRow({
  label,
  hint,
  htmlFor,
  children,
}: {
  label: string;
  hint?: string;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div className="form-row">
      <label className="form-label" htmlFor={htmlFor}>
        {label}
      </label>
      <div className="form-control">
        {children}
        {hint !== undefined && <p className="form-hint">{hint}</p>}
      </div>
    </div>
  );
}
