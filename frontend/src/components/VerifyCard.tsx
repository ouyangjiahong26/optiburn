import type { VerifyReport } from "../types";

// 校验结果卡片：通过给一句说明，未通过列出中文差异。两个盘片页面共用。
export function VerifyCard({
  report,
  passNote,
}: {
  report: VerifyReport;
  passNote: string;
}) {
  return (
    <section className="result-card">
      <h2>{report.differences.length === 0 ? "校验通过" : "校验未通过"}</h2>
      {report.differences.length === 0 ? (
        <p className="result-note">{passNote}</p>
      ) : (
        <ul className="diff-list">
          {report.differences.map((line, index) => (
            <li key={index}>{line}</li>
          ))}
        </ul>
      )}
    </section>
  );
}
