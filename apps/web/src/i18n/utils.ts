// Astro i18n helper — official "useTranslations" recipe pattern.
// https://docs.astro.build/en/recipes/i18n/

import { defaultLang, ui } from "./ui";

export type Lang = keyof typeof ui;

export function getLangFromUrl(url: URL): Lang {
  const [, maybeLang] = url.pathname.split("/");
  if (maybeLang in ui) {
    return maybeLang as Lang;
  }
  return defaultLang;
}

export function useTranslations(lang: Lang) {
  return function t<K extends keyof (typeof ui)[typeof defaultLang]>(key: K) {
    return ui[lang][key] ?? ui[defaultLang][key];
  };
}

// Strips the locale prefix from a pathname, returning the remainder
// (no leading slash) so it can be fed back into `astro:i18n`'s
// `getRelativeLocaleUrl` / `getAbsoluteLocaleUrl` as the `path` argument.
export function getPathWithoutLocale(url: URL, lang: Lang): string {
  const { pathname } = url;
  if (lang === defaultLang) {
    return pathname.replace(/^\//, "");
  }
  return pathname.replace(new RegExp(`^/${lang}/?`), "");
}
