import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'optiburn',
  tagline: '把文件刻进光盘',
  favicon: 'img/favicon.png',

  future: {
    v4: true,
  },

  url: 'https://ouyangjiahong26.github.io',
  baseUrl: '/optiburn/',
  trailingSlash: false,

  organizationName: 'ouyangjiahong26',
  projectName: 'optiburn',

  onBrokenLinks: 'throw',

  i18n: {
    defaultLocale: 'zh-Hans',
    locales: ['zh-Hans'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          editUrl: 'https://github.com/ouyangjiahong26/optiburn/tree/main/docs-site/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    metadata: [
      {
        name: 'description',
        content:
          'optiburn：Rust 写的跨平台光盘刻录工具。同时支持 Windows 与 Linux、x86_64 与 arm64。面向 Ubuntu、银河麒麟等 Linux 桌面解决刻录难与不稳定：写前检查、实时进度、任务可中止、失败原因中文归类、写后回读校验。',
      },
    ],
    image: 'img/screenshot-burn.png',
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: true,
    },
    navbar: {
      hideOnScroll: false,
      title: 'optiburn',
      logo: {
        alt: 'optiburn',
        src: 'img/logo.svg',
        srcDark: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: '文档',
        },
        {
          href: 'https://github.com/ouyangjiahong26/optiburn',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: '文档',
          items: [
            {label: '简介', to: '/docs/intro'},
            {label: '快速开始', to: '/docs/quick-start'},
            {label: '命令行用法', to: '/docs/usage'},
            {label: '图形界面', to: '/docs/gui'},
          ],
        },
        {
          title: '深入',
          items: [
            {label: '介质与文件系统', to: '/docs/compatibility'},
            {label: '架构', to: '/docs/architecture'},
            {label: '常见问题', to: '/docs/faq'},
          ],
        },
        {
          title: '链接',
          items: [
            {label: 'GitHub', href: 'https://github.com/ouyangjiahong26/optiburn'},
            {label: 'Releases', href: 'https://github.com/ouyangjiahong26/optiburn/releases'},
            {
              label: 'Windows 可读性',
              href: 'https://github.com/ouyangjiahong26/optiburn/blob/main/docs/WINDOWS-COMPAT.md',
            },
          ],
        },
      ],
      copyright: `Copyright ${new Date().getFullYear()} optiburn contributors. MIT License.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
