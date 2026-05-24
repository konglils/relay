(function () {
  function flashCopiedState(button) {
    button.textContent = "已复制";
    setTimeout(() => {
      button.textContent = "复制链接";
    }, 1200);
  }

  function fallbackCopyText(text) {
    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    textarea.style.pointerEvents = "none";
    document.body.appendChild(textarea);
    textarea.focus();
    textarea.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch (_) {}
    document.body.removeChild(textarea);
    return ok;
  }

  function initSpeedtest() {
    const btn = document.getElementById("speedtest");
    if (!btn) return;

    btn.addEventListener("click", async () => {
      btn.disabled = true;
      const original = btn.textContent;
      btn.textContent = "测速中";
      const start = performance.now();
      let received = 0;
      const controller = new AbortController();
      setTimeout(() => controller.abort(), 10000);

      try {
        const res = await fetch("/speedtest", {
          signal: controller.signal,
          cache: "no-store",
        });
        const reader = res.body && res.body.getReader ? res.body.getReader() : null;
        if (reader) {
          while (true) {
            const { done, value } = await reader.read();
            if (done) break;
            received += value.byteLength;
          }
        }
      } catch (_) {}

      const end = performance.now();
      const seconds = (end - start) / 1000;
      const mbps = (received * 8) / (seconds * 1000 * 1000);
      alert(
        `接收 ${received} 字节，耗时 ${seconds.toFixed(2)} 秒，速度约 ${mbps.toFixed(2)} Mbps`,
      );
      btn.textContent = original;
      btn.disabled = false;
    });
  }

  function initQr() {
    const qrModal = document.getElementById("qr-modal");
    if (!qrModal) return;

    const qrTitle = document.getElementById("qr-title");
    const qrCode = document.getElementById("qr-code");
    const qrClose = document.getElementById("qr-close");
    const qrCopy = document.getElementById("qr-copy");
    if (!qrTitle || !qrCode || !qrClose || !qrCopy) return;

    let currentUrl = "";

    function openQr(label, href) {
      if (typeof qrcode !== "function") {
        alert("二维码库加载失败，请刷新页面重试。");
        return;
      }
      const target = new URL(href, window.location.origin);
      const loc = target.searchParams.get("loc") || "/";
      const normalized = new URL("/file", window.location.origin);
      normalized.search = new URLSearchParams({ loc }).toString();
      currentUrl = normalized.toString();
      qrTitle.textContent = label;
      const qr = qrcode(0, "M");
      qr.addData(currentUrl);
      qr.make();
      qrCode.innerHTML = qr.createSvgTag({ scalable: true, margin: 0 });
      qrModal.classList.add("open");
    }

    function closeQr() {
      qrModal.classList.remove("open");
      qrCode.innerHTML = "";
      currentUrl = "";
    }

    document.querySelectorAll(".qr-btn").forEach((btn) => {
      btn.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        openQr(btn.dataset.label || "", btn.dataset.href || "/");
      });
    });

    qrClose.addEventListener("click", closeQr);
    qrModal.addEventListener("click", (event) => {
      if (event.target === qrModal) closeQr();
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && qrModal.classList.contains("open")) closeQr();
    });
    qrCopy.addEventListener("click", async () => {
      if (!currentUrl) return;
      try {
        if (navigator.clipboard && navigator.clipboard.writeText) {
          await navigator.clipboard.writeText(currentUrl);
          flashCopiedState(qrCopy);
          return;
        }
      } catch (_) {}

      if (fallbackCopyText(currentUrl)) {
        flashCopiedState(qrCopy);
      }
    });
  }

  initSpeedtest();
  initQr();
})();
