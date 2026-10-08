// 制作镜像：等待整个任务结束才拿到结果，运行形态只有不确定进度一种。
import { useState } from "react";
import type { FormEvent } from "react";
import { startBuildImage } from "../api";
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

export function BuildPage({ locked, onJobStart, onJobAbort }: JobControls & { locked: boolean }) {
  const [src, setSrc] = useState("");
  const [output, setOutput] = useState("");
  const [profile, setProfile] = useState<DiscProfile>("dvd");
  const [volumeId, setVolumeId] = useState("OPTIBURN");
  const [imageInfo, setImageInfo] = useState<ImageInfoDto | null>(null);

  const defaultHint = output.trim() === "" ? defaultOutputHint(src) : null;

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
          <h1>制作镜像</h1>
          <p>把一个目录打包成 ISO 镜像文件。</p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        <FormRow label="源目录">
          <PathField
            mode="directory"
            value={src}
            onChange={setSrc}
            disabled={locked}
            placeholder="选择要打包的目录"
          />
        </FormRow>
        <FormRow
          label="输出镜像"
          hint={defaultHint === null ? undefined : `留空时保存为 ${defaultHint}`}
        >
          <PathField
            mode="saveIso"
            value={output}
            onChange={setOutput}
            disabled={locked}
            placeholder="选择保存位置"
          />
        </FormRow>
        <FormRow label="介质">
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
        <FormRow label="卷标" htmlFor="build-volume">
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
            开始制作
          </button>
        </div>
      </form>
      {imageInfo !== null && (
        <section className="result-card">
          <h2>制作完成</h2>
          <dl className="result-list">
            <div>
              <dt>扇区</dt>
              <dd>{imageInfo.sectors.toLocaleString("zh-CN")}</dd>
            </div>
            <div>
              <dt>字节</dt>
              <dd>{imageInfo.bytes.toLocaleString("zh-CN")} 字节</dd>
            </div>
            <div>
              <dt>文件系统</dt>
              <dd>{imageInfo.filesystems.join("、")}</dd>
            </div>
          </dl>
        </section>
      )}
    </div>
  );
}
