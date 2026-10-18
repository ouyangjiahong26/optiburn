import type { VerifyReport } from "../types";
import { t } from "../i18n";

// 校验结果卡片：通过给一句说明，未通过分别列出读不出的文件与内容差异（随界面语言）。
// 两个盘片页面共用。
export function VerifyCard({
  report,
  passNote,
}: {
  report: VerifyReport;
  passNote: string;
}) {
  const failed = report.differences.length > 0 || report.unreadable.length > 0;
  return (
    <section className="result-card">
      <h2>{failed ? t("Verification failed", "校验未通过") : t("Verification passed", "校验通过")}</h2>
      {!failed && <p className="result-note">{passNote}</p>}
      {report.unreadable.length > 0 && (
        <>
          <p className="result-note">{t("Unreadable files", "读不出的文件")}</p>
          <ul className="diff-list">
            {report.unreadable.map((line, index) => (
              <li key={index}>{line}</li>
            ))}
          </ul>
        </>
      )}
      {report.differences.length > 0 && (
        <ul className="diff-list">
          {report.differences.map((line, index) => (
            <li key={index}>{line}</li>
          ))}
        </ul>
      )}
    </section>
  );
}
