# Scribe documentation site

This directory contains the Astro + Starlight documentation site.

```bash
npm ci
npm run dev
npm run check
npm run build
npm run preview
```

By default, production builds target GitHub Pages at `https://tyhuang9.github.io/scribe/`. Set `SITE_URL` and `BASE_PATH` for another host or path. For example, use `SITE_URL=https://docs.example.com BASE_PATH=/` for a root deployment.

Keep product facts aligned with the checked-in application code and root `README.md`. This site is a concise guide, not a replacement for implementation records.

## GitHub Pages deployment and maintenance

Before the first deployment, open **Settings → Pages** in the repository and set **Source** to **GitHub Actions**. The workflow's default project-site settings are `SITE_URL=https://tyhuang9.github.io` and `BASE_PATH=/scribe`, which publish to `https://tyhuang9.github.io/scribe/`. For a root deployment, set `SITE_URL` to the chosen origin and `BASE_PATH=/` in `.github/workflows/docs.yml`.

To use a custom domain, verify the domain first, configure the domain and its DNS records as described in GitHub's [custom-domain guidance](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site), then update `SITE_URL` and `BASE_PATH` for the chosen root or project deployment. Do not add a `CNAME` file without a real verified domain. The current custom GitHub Actions workflow does not require a `CNAME` file; add one only after verification if a future publishing process requires it.
