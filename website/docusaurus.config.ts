import type {Config} from '@docusaurus/types';
import type {Options, ThemeConfig} from '@docusaurus/preset-classic';
import {themes as prismThemes} from 'prism-react-renderer';
import {readFileSync} from 'node:fs';
import path from 'node:path';

const siteUrl = process.env.SITE_URL ?? 'https://agentic-os.sh';
const baseUrl = process.env.BASE_URL ?? '/';

const config: Config = {
  title: 'ANOLISA',
  tagline: 'A multi-component system software stack for AI Agent workloads.',
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
  plugins: ['./plugins/document-redirects.mjs'],
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
          async sidebarItemsGenerator({defaultSidebarItemsGenerator, ...args}) {
            const items = await defaultSidebarItemsGenerator(args);
            if (/^user-guide\/[^/]+$/.test(args.item.dirName)) {
              // The component category already links to its manual entry.
              return items.filter((item) => item.type !== 'doc' || item.id !== `${args.item.dirName}/index`);
            }
            return items;
          },
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
          'ANOLISA is a multi-component system software stack providing context management, Skills delivery, workspace recovery, security, and observability for AI Agent applications, frameworks, and platforms.',
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
        ...JSON.parse(readFileSync(path.join(__dirname, '.generated/navbar-items.json'), 'utf8')),
        {type: 'localeDropdown', position: 'right'},
      ],
    },
    footer: {
      style: 'dark',
      links: JSON.parse(readFileSync(path.join(__dirname, '.generated/footer-links.json'), 'utf8')),
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
