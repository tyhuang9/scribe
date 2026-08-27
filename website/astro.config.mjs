import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: process.env.SITE_URL ?? 'https://tyhuang9.github.io',
  base: process.env.BASE_PATH ?? '/scribe',
  integrations: [
    starlight({
      title: 'Scribe documentation',
      description: 'Lightning-fast local transcription that stays out of your way.',
      disable404Route: true,
      logo: {
        light: './src/assets/scribe-header-light.svg',
        dark: './src/assets/scribe-header-dark.svg',
        alt: '',
        replacesTitle: true
      },
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/tyhuang9/scribe' }],
      sidebar: [
        { label: 'Overview', link: '/' },
        {
          label: 'Getting started',
          items: [
            { label: 'Install and run', link: '/install-and-run/' },
            { label: 'First transcription', link: '/first-transcription/' },
            { label: 'Hotkeys and recording', link: '/hotkeys-and-recording/' }
          ]
        },
        {
          label: 'Using Scribe',
          items: [
            { label: 'Models and runtimes', link: '/models-and-runtimes/' },
            { label: 'History, output, and privacy', link: '/history-output-and-privacy/' },
            { label: 'Settings and environment', link: '/settings-and-environment/' }
          ]
        },
        {
          label: 'Platforms',
          items: [
            { label: 'Windows', link: '/platforms/windows/' },
            { label: 'Linux and WSL', link: '/platforms/linux-wsl/' },
            { label: 'macOS', link: '/platforms/macos/' },
            { label: 'Local-first and permissions', link: '/local-first-and-permissions/' }
          ]
        },
        {
          label: 'Reference',
          items: [
            { label: 'Troubleshooting', link: '/troubleshooting/' },
            { label: 'Development', link: '/development/' },
            { label: 'Project status and reference', link: '/project-status/' }
          ]
        }
      ],
      customCss: ['./src/styles/custom.css']
    })
  ]
});
