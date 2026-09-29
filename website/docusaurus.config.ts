import type {Config} from '@docusaurus/types';
import type {Options, ThemeConfig} from '@docusaurus/preset-classic';
import {themes as prismThemes} from 'prism-react-renderer';

const siteUrl = process.env.SITE_URL ?? 'https://agentic-os.sh';
const baseUrl = process.env.BASE_URL ?? '/';

const config: Config = {
  title: 'ANOLISA',
  tagline: 'The operating layer for agent workloads',
  url: siteUrl,
  baseUrl,
  organizationName: 'agentic-os-org',
  projectName: 'ANOLISA',
  favicon: 'img/brand/favicon.ico',
  trailingSlash: true,
  onBrokenLinks: 'throw',
  headTags: [
    {
      tagName: 'link',
      attributes: {
        rel: 'icon',
        type: 'image/png',
        sizes: '16x16',
        href: `${baseUrl}img/brand/favicon-16.png`,
      },
    },
    {
      tagName: 'link',
      attributes: {
        rel: 'icon',
        type: 'image/png',
        sizes: '32x32',
        href: `${baseUrl}img/brand/favicon-32.png`,
      },
    },
    {
      tagName: 'link',
      attributes: {
        rel: 'icon',
        type: 'image/png',
        sizes: '48x48',
        href: `${baseUrl}img/brand/favicon-48.png`,
      },
    },
  ],
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
  },
  staticDirectories: ['static', '.generated/static'],
  i18n: {
    defaultLocale: 'en',
    locales: ['en', 'zh'],
    path: '.generated/i18n',
    localeConfigs: {
      en: {label: 'English', htmlLang: 'en'},
      zh: {label: '中文', htmlLang: 'zh-CN'},
    },
  },
  presets: [
    [
      'classic',
      {
        docs: {
          path: '.generated/docs',
          routeBasePath: 'docs',
          sidebarPath: './sidebars.ts',
          showLastUpdateAuthor: false,
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Options,
    ],
  ],
  themeConfig: {
    metadata: [
      {name: 'theme-color', content: '#0b1220'},
      {
        name: 'description',
        content:
          'ANOLISA is a local-first operating layer for building, running, securing, and observing agent workloads.',
      },
    ],
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'ANOLISA',
      logo: {
        alt: 'ANOLISA',
        src: 'img/brand/anolisa-glyph-light.svg',
        srcDark: 'img/brand/anolisa-glyph-dark.svg',
        width: 30,
        height: 30,
      },
      hideOnScroll: false,
      items: [
        {to: '/docs/user-guide', label: 'User Guide', position: 'left'},
        {to: '/docs/developer-guide', label: 'Developer Guide', position: 'left'},
        {to: '/changelog', label: 'Changelog', position: 'left'},
        {to: '/agents/', label: 'For Agents', position: 'left'},
        {
          href: 'https://github.com/agentic-os-org/ANOLISA',
          label: 'GitHub',
          position: 'right',
          className: 'headerGithubLink',
          'aria-label': 'ANOLISA GitHub repository',
        },
        {type: 'localeDropdown', position: 'right'},
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            {label: 'Documentation', to: '/docs'},
            {label: 'Quickstart', to: '/docs/quickstart'},
            {label: 'Building', to: '/docs/building'},
            {label: 'Changelog', to: '/changelog'},
            {label: 'For Agents', to: '/agents/'},
          ],
        },
        {
          title: 'Guides',
          items: [
            {label: 'User Guide', to: '/docs/user-guide'},
            {label: 'Developer Guide', to: '/docs/developer-guide'},
          ],
        },
        {
          title: 'Community',
          items: [
            {label: 'GitHub', href: 'https://github.com/agentic-os-org/ANOLISA'},
            {
              label: 'Contributing',
              href: 'https://github.com/agentic-os-org/ANOLISA/blob/main/CONTRIBUTING.md',
            },
            {
              label: 'Security',
              href: 'https://github.com/agentic-os-org/ANOLISA/blob/main/SECURITY.md',
            },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} ANOLISA contributors. Apache-2.0.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['bash', 'json', 'toml'],
    },
  } satisfies ThemeConfig,
};

export default config;
