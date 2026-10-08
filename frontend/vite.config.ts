import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 端口与 devUrl 绑定：被占用时直接失败，避免 Tauri 连到一个陌生的前端。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // 明确绑定 IPv4：devUrl 写的是 127.0.0.1，localhost 在部分系统解析到 ::1 会连不上。
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "es2022",
  },
});
