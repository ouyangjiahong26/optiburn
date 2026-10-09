import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    {
      type: 'doc',
      id: 'intro',
      label: '简介',
    },
    {
      type: 'category',
      label: '开始使用',
      items: ['quick-start', 'usage', 'gui'],
      collapsed: false,
    },
    {
      type: 'category',
      label: '深入',
      items: ['compatibility', 'architecture', 'faq'],
      collapsed: false,
    },
  ],
};

export default sidebars;
