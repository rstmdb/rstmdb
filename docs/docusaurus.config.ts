import { themes as prismThemes } from "prism-react-renderer";
import type { Config } from "@docusaurus/types";
import type * as Preset from "@docusaurus/preset-classic";

const config: Config = {
  title: "rstmdb",
  tagline: "State Machine Database with WAL Durability",
  favicon: "img/favicon.ico",

  future: {
    v4: true,
  },

  // GitHub Pages deployment
  url: "https://docs.rstmdb.com",
  baseUrl: "/",

  organizationName: "rstmdb",
  projectName: "rstmdb",
  deploymentBranch: "gh-pages",
  trailingSlash: false,

  onBrokenLinks: "throw",
  onBrokenMarkdownLinks: "warn",

  i18n: {
    defaultLocale: "en",
    locales: ["en"],
  },

  presets: [
    [
      "classic",
      {
        docs: {
          sidebarPath: "./sidebars.ts",
          editUrl: "https://github.com/rstmdb/rstmdb/tree/main/docs/",
          routeBasePath: "/", // Docs as homepage
        },
        blog: false,
        theme: {},
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: "img/rstmdb-social-card.png",
    colorMode: {
      defaultMode: "dark",
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: "rstmdb",
      logo: {
        alt: "rstmdb Logo",
        src: "img/logo.svg",
      },
      items: [
        {
          href: "https://github.com/rstmdb/rstmdb",
          label: "GitHub",
          position: "right",
        },
      ],
    },
    footer: {
      style: "dark",
      links: [
        {
          title: "Documentation",
          items: [
            {
              label: "Getting Started",
              to: "/getting-started",
            },
            {
              label: "Architecture",
              to: "/architecture",
            },
            {
              label: "Protocol Reference",
              to: "/protocol/overview",
            },
          ],
        },
        {
          title: "Resources",
          items: [
            {
              label: "CLI Reference",
              to: "/cli",
            },
            {
              label: "Configuration",
              to: "/configuration",
            },
            {
              label: "API Reference",
              to: "/api/commands",
            },
          ],
        },
        {
          title: "More",
          items: [
            {
              label: "GitHub",
              href: "https://github.com/rstmdb/rstmdb",
            },
            {
              label: "Releases",
              href: "https://github.com/rstmdb/rstmdb/releases",
            },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} rstmdb Authors. Licensed under BSL-1.1.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ["bash", "json", "rust", "yaml", "toml"],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
