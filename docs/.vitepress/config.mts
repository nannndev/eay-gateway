import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'eay-gateway',
  description: 'One local, OpenAI-compatible endpoint for all your AI subscriptions and API keys.',
  base: process.env.DOCS_BASE || '/',
  appearance: 'force-dark',
  head: [
    ['link', { rel: 'icon', href: '/logo.svg' }],
    ['meta', { name: 'theme-color', content: '#050505' }],
  ],
  themeConfig: {
    logo: '/logo.svg',
    siteTitle: 'eay-gateway',
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Getting Started', link: '/getting-started' },
      {
        text: 'Guides',
        items: [
          { text: 'Routing & Failover', link: '/routing' },
          { text: 'Providers & Logins', link: '/providers' },
          { text: 'Configuration', link: '/configuration' },
        ],
      },
      {
        text: 'Reference',
        items: [
          { text: 'HTTP API', link: '/api' },
          { text: 'Architecture', link: '/architecture' },
          { text: 'Development', link: '/development' },
          { text: 'Changelog', link: '/changelog' },
        ],
      },
    ],
    sidebar: [
      {
        text: 'Getting Started',
        items: [
          { text: 'Quick Start', link: '/getting-started' },
          { text: 'Configuration', link: '/configuration' },
        ],
      },
      {
        text: 'Core Features',
        items: [
          { text: 'Routing & Auto Modes', link: '/routing' },
          { text: 'Supported Providers', link: '/providers' },
        ],
      },
      {
        text: 'Technical Reference',
        items: [
          { text: 'HTTP & Admin API', link: '/api' },
          { text: 'Architecture & Storage', link: '/architecture' },
          { text: 'Development & Testing', link: '/development' },
          { text: 'Changelog', link: '/changelog' },
        ],
      },
    ],
    socialLinks: [
      { icon: 'github', link: 'https://github.com/nannndev/eay-gateway' },
    ],
    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright © 2026 eay-gateway contributors',
    },
    search: {
      provider: 'local',
    },
  },
})
