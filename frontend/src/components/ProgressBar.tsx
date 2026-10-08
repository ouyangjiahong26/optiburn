// fraction 为 null 表示不确定形态（制作镜像没有进度回调），否则为 0.0 到 1.0。
export function ProgressBar({ fraction }: { fraction: number | null }) {
  if (fraction === null) {
    return (
      <div className="progress progress-indeterminate" role="progressbar" />
    );
  }
  const clamped = Math.min(1, Math.max(0, fraction));
  return (
    <div
      className="progress"
      role="progressbar"
      aria-valuenow={Math.round(clamped * 100)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <div className="progress-fill" style={{ width: `${clamped * 100}%` }} />
    </div>
  );
}
