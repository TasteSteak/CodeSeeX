(function () {
  const supportedLanguages = ["en_us", "zh_cn"];
  const fallbackLanguage = "en_us";
  const storageKey = "codeseex.website.language";

  function normalizeLanguage(value) {
    const normalized = String(value || "").toLowerCase().replace("-", "_");
    if (normalized.startsWith("zh")) {
      return "zh_cn";
    }
    return supportedLanguages.includes(normalized) ? normalized : fallbackLanguage;
  }

  function preferredLanguage() {
    try {
      const saved = window.localStorage.getItem(storageKey);
      if (saved) {
        return normalizeLanguage(saved);
      }
    } catch (_) {
      // localStorage can be blocked for file URLs in some hardened environments.
    }
    return normalizeLanguage(navigator.language || fallbackLanguage);
  }

  function readTranslation(lang, key) {
    const dictionaries = window.CodeSeeXWebsiteI18n || {};
    return (
      dictionaries[lang]?.[key] ??
      dictionaries[fallbackLanguage]?.[key] ??
      null
    );
  }

  function applyLanguage(lang) {
    const language = normalizeLanguage(lang);
    document.documentElement.lang = language === "zh_cn" ? "zh-CN" : "en";
    document.documentElement.dataset.language = language;

    document.querySelectorAll("[data-i18n]").forEach((node) => {
      const value = readTranslation(language, node.dataset.i18n);
      if (value !== null) {
        node.textContent = value;
      }
    });

    document.querySelectorAll("[data-i18n-alt]").forEach((node) => {
      const value = readTranslation(language, node.dataset.i18nAlt);
      if (value !== null) {
        node.setAttribute("alt", value);
      }
    });

    document.querySelectorAll("[data-i18n-title]").forEach((node) => {
      const value = readTranslation(language, node.dataset.i18nTitle);
      if (value !== null) {
        node.setAttribute("title", value);
      }
    });

    document.querySelectorAll("[data-language-choice]").forEach((button) => {
      const selected = button.dataset.languageChoice === language;
      button.setAttribute("aria-pressed", selected ? "true" : "false");
    });

    try {
      window.localStorage.setItem(storageKey, language);
    } catch (_) {
      // Non-critical persistence failure.
    }
  }

  function setupLanguageSwitch() {
    document.querySelectorAll("[data-language-choice]").forEach((button) => {
      button.addEventListener("click", () => {
        applyLanguage(button.dataset.languageChoice);
      });
    });
  }

  function setupScreenshotTabs() {
    const triggers = Array.from(document.querySelectorAll("[data-screen-trigger]"));
    const panels = Array.from(document.querySelectorAll("[data-screen-panel]"));
    if (!triggers.length || !panels.length) {
      return;
    }

    triggers.forEach((trigger) => {
      trigger.addEventListener("click", () => {
        const target = trigger.dataset.screenTrigger;
        triggers.forEach((candidate) => {
          candidate.setAttribute(
            "aria-selected",
            candidate.dataset.screenTrigger === target ? "true" : "false",
          );
        });
        panels.forEach((panel) => {
          panel.hidden = panel.dataset.screenPanel !== target;
        });
      });
    });
  }

  function markCurrentYear() {
    document.querySelectorAll("[data-current-year]").forEach((node) => {
      node.textContent = String(new Date().getFullYear());
    });
  }

  document.addEventListener("DOMContentLoaded", () => {
    setupLanguageSwitch();
    setupScreenshotTabs();
    markCurrentYear();
    applyLanguage(preferredLanguage());
  });
})();
