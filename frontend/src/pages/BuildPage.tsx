// 制作镜像：等待整个任务结束才拿到结果，运行形态只有不确定进度一种。
import { useState } from "react";
import type { FormEvent } from "react";
import { startBuildImage } from "../api";
import { t } from "../i18n";
import { FormRow } from "../components/FormRow";
import { PathField } from "../components/PathField";
import type { DiscProfile, ImageInfoDto, JobControls } from "../types";

const PROFILE_LABEL: Record<DiscProfile, string> = {
  cd: "CD",
  dvd: "DVD",
  bd: "BD",
};

// 只做提示：按两种分隔符取“父目录 + 目录名 + .iso”，源为根目录时退回
// optiburn.iso；真正的默认值由后端计算，这里不保证与之一致。
function defaultOutputHint(src: string): string | null {
  const trimmed = src.trim();
  if (trimmed === "") {
    return null;
  }
  const stripped = trimmed.replace(/[\\/]+$/, "");
  if (stripped === "") {
    return "optiburn.iso";
  }
  const cut = Math.max(stripped.lastIndexOf("/"), stripped.lastIndexOf("\\"));
  if (cut < 0) {
    return `${stripped}.iso`;
  }
  return `${stripped.slice(0, cut)}${stripped[cut]}${stripped.slice(cut + 1)}.iso`;
}

// 用户填写的输出没有扩展名时，后端 start_build_image 会补 .iso；这里提前展示补全后
// 的名字，让用户提交前就看到最终保存位置。判断规则对齐后端的 Path::extension：
// 文件名里最后一个点出现在开头之前才算有扩展名。同样只是提示，不保证与后端逐字
// 一致。
function isoSuffixHint(output: string): string | null {
  const stripped = output.trim().replace(/[\\/]+$/, "");
  if (stripped === "") {
    return null;
  }
  const cut = Math.max(stripped.lastIndexOf("/"), stripped.lastIndexOf("\\"));
  const name = stripped.slice(cut + 1);
  if (name.lastIndexOf(".") > 0) {
    return null;
  }
  return `${stripped}.iso`;
}

export function BuildPage({ locked, onJobStart, onJobAbort }: JobControls & { locked: boolean }) {
  const [src, setSrc] = useState("");
  const [output, setOutput] = useState("");
  const [profile, setProfile] = useState<DiscProfile>("dvd");
  const [volumeId, setVolumeId] = useState("OPTIBURN");
  const [imageInfo, setImageInfo] = useState<ImageInfoDto | null>(null);

  const defaultHint = output.trim() === "" ? defaultOutputHint(src) : null;
  const suffixHint = defaultHint !== null ? null : isoSuffixHint(output);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setImageInfo(null);
    onJobStart("build");
    try {
      setImageInfo(
        await startBuildImage({
          src: src.trim(),
          output: output.trim() === "" ? null : output.trim(),
          profile,
          volumeId: volumeId.trim(),
        }),
      );
    } catch (cause) {
      // 任务真实失败时 job-done 事件会展示原因，这里只在启动被拒绝时解锁。
      onJobAbort("build", String(cause));
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>{t("Create Image", "制作镜像")}</h1>
          <p>{t("Pack a directory into an ISO image file.", "把一个目录打包成 ISO 镜像文件。")}</p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        <FormRow label={t("Source directory", "源目录")}>
          <PathField
            mode="directory"
            value={src}
            onChange={setSrc}
            disabled={locked}
            placeholder={t("Choose a directory to pack", "选择要打包的目录")}
          />
        </FormRow>
        <FormRow
          label={t("Output image", "输出镜像")}
          hint={
            defaultHint !== null
              ? t(`Defaults to ${defaultHint}`, `留空时保存为 ${defaultHint}`)
              : suffixHint !== null
                ? t(`Will be saved as ${suffixHint}`, `将保存为 ${suffixHint}`)
                : undefined
          }
        >
          <PathField
            mode="saveIso"
            value={output}
            onChange={setOutput}
            disabled={locked}
            placeholder={t("Choose where to save", "选择保存位置")}
          />
        </FormRow>
        <FormRow label={t("Media", "介质")}>
          <div className="radio-group">
            {(["cd", "dvd", "bd"] as const).map((value) => (
              <label key={value} className="option">
                <input
                  type="radio"
                  name="build-profile"
                  checked={profile === value}
                  disabled={locked}
                  onChange={() => setProfile(value)}
                />
                {PROFILE_LABEL[value]}
              </label>
            ))}
          </div>
        </FormRow>
        <FormRow label={t("Volume label", "卷标")} htmlFor="build-volume">
          <input
            id="build-volume"
            type="text"
            className="text-input"
            value={volumeId}
            disabled={locked}
            onChange={(event) => setVolumeId(event.target.value)}
          />
        </FormRow>
        <div className="form-actions">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={locked || src.trim() === ""}
          >
            {t("Create", "开始制作")}
          </button>
        </div>
      </form>
      {imageInfo !== null && (
        <section className="result-card">
          <h2>{t("Image created", "制作完成")}</h2>
          <dl className="result-list">
            <div>
              <dt>{t("Sectors", "扇区")}</dt>
              <dd>{imageInfo.sectors.toLocaleString("zh-CN")}</dd>
            </div>
            <div>
              <dt>{t("Bytes", "字节")}</dt>
              <dd>
                {imageInfo.bytes.toLocaleString("zh-CN")} {t("bytes", "字节")}
              </dd>
            </div>
            <div>
              <dt>{t("File systems", "文件系统")}</dt>
              <dd>{imageInfo.filesystems.join(t(", ", "、"))}</dd>
            </div>
          </dl>
        </section>
      )}
    </div>
  );
}
