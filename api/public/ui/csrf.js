(() => {
  (() => {
    const safe = /^(GET|HEAD|OPTIONS)$/i;
    const nativeFetch = window.fetch.bind(window);
    window.ndimonCsrf = "";
    window.fetch = (input, init = {}) => {
      const req = input instanceof Request ? input : null;
      const method = init.method || req && req.method || "GET";
      const url = new URL(req ? req.url : String(input), location.href);
      if (window.ndimonCsrf && !safe.test(method) && url.origin === location.origin) {
        const headers = new Headers(init.headers || req && req.headers || void 0);
        if (!headers.has("X-CSRF-Token")) headers.set("X-CSRF-Token", window.ndimonCsrf);
        init = { ...init, headers };
      }
      return nativeFetch(input, init);
    };
  })();
})();
