// static/js/splash.js

document.addEventListener("alpine:init", () => {
  // ── TERMINAL LOGIC ─────────────────────────────────────────────────────
  Alpine.data("lmTerminal", () => ({
    lang: 0,
    titles: ["main.go", "main.rs", "main.zig"],
    deployStatus: "READY",
    deploying: false,
    deployNames: ["my-app", "my-api", "my-fn"],

    samples: [
      // Go
      'package main\n\nimport (\n\t"fmt"\n\t"net/http"\n)\n\nfunc handler(\n\tw http.ResponseWriter,\n\tr *http.Request,\n) {\n\tfmt.Fprintln(w, "hello")\n}\n\nfunc main() {\n\thttp.HandleFunc("/", handler)\n\thttp.ListenAndServe(":8080", nil)\n}',
      // Rust
      'use std::net::TcpListener;\nuse std::io::Write;\n\nfn main() {\n\tlet ln = TcpListener\n\t\t::bind("0.0.0.0:8080").unwrap();\n\tfor stream in ln.incoming() {\n\t\tlet mut s = stream.unwrap();\n\t\tlet body = b"HTTP/1.1 200 OK\\r\\n\\r\\nhello";\n\t\ts.write_all(body).ok();\n\t}\n}',
      // Zig
      'const std = @import("std");\nconst net = std.net;\n\npub fn main() !void {\n\tconst addr = try net.Address\n\t\t.resolveIp("0.0.0.0", 8080);\n\tvar srv = try addr.listen(.{});\n\tdefer srv.deinit();\n\twhile (true) {\n\t\tconst conn = try srv.accept();\n\t\t_ = conn.stream.write("hello");\n\t\tconn.stream.close();\n\t}\n}',
    ],

    configs: [
      // Go — Metal
      '[service]\nname   = "my-app"\nengine = "metal"\nport   = 8080\n\n[metal]\nvcpu      = 1\nmemory_mb = 128',
      // Rust — Metal
      '[service]\nname   = "my-api"\nengine = "metal"\nport   = 8080\n\n[metal]\nvcpu      = 2\nmemory_mb = 256',
      // Zig — Liquid
      '[service]\nname   = "my-fn"\nengine = "liquid"\n\n[liquid]\nwasm = "main.wasm"',
    ],

    _timer: null,

    init() {
      this.startTimer();
    },

    // Alpine calls this automatically when HTMX removes the terminal from the DOM
    destroy() {
      if (this._timer) clearInterval(this._timer);
    },

    startTimer() {
      this._timer = setInterval(() => {
        this.lang = (this.lang + 1) % 3;
      }, 3500);
    },

    switchLang(n) {
      this.lang = n;
      clearInterval(this._timer);
      this.startTimer();
    },
  }));

  // ── CANVAS ANIMATION LOGIC ─────────────────────────────────────────────
  Alpine.data("lightningCanvas", () => ({
    c: null,
    ctx: null,
    raf: null,
    W: 0,
    H: 0,
    frame: 0,
    strikes: [],
    pulses: [],
    GAP: 44,
    COLS: 0,
    ROWS: 0,
    GX: 0,
    GY: 0,

    // Bind the resize event so we can easily remove it later
    handleResize() {
      this.rs();
    },

    init() {
      this.c = this.$el; // Alpine gives us the exact canvas element via $el
      this.ctx = this.c.getContext("2d");

      // Re-bind `this` context for the event listener
      this.handleResize = this.handleResize.bind(this);
      window.addEventListener("resize", this.handleResize);

      this.rs();
      for (let s = 0; s < 8; s++) this.addStrike();
      this.tick();
    },

    // Clean up the animation loop and event listener when HTMX navigates away
    destroy() {
      if (this.raf) cancelAnimationFrame(this.raf);
      window.removeEventListener("resize", this.handleResize);
    },

    rs() {
      let b = this.c.getBoundingClientRect();
      this.W = b.width;
      this.H = b.height;
      this.c.width = Math.round(this.W * devicePixelRatio);
      this.c.height = Math.round(this.H * devicePixelRatio);
      this.ctx.setTransform(devicePixelRatio, 0, 0, devicePixelRatio, 0, 0);
      this.COLS = Math.round(this.W / this.GAP) || 1;
      this.ROWS = Math.round(this.H / this.GAP) || 1;
      this.GX = this.W / this.COLS;
      this.GY = this.H / this.ROWS;
    },

    addStrike() {
      if (Math.random() < 0.35) {
        this.strikes.push({
          h: true,
          row: Math.floor(Math.random() * this.ROWS),
          pos: 0,
          spd: 0.8 + Math.random() * 1.2,
          len: 3 + Math.floor(Math.random() * 4),
        });
      } else {
        this.strikes.push({
          h: false,
          col: Math.floor(Math.random() * this.COLS),
          pos: 0,
          spd: 0.8 + Math.random() * 1.2,
          len: 3 + Math.floor(Math.random() * 4),
        });
      }
    },

    addPulse() {
      let col = Math.floor(Math.random() * this.COLS);
      let row = Math.floor(Math.random() * this.ROWS);
      this.pulses.push({
        cx: col * this.GX,
        cy: row * this.GY,
        r: 0,
        maxR: Math.min(this.W, this.H) * 0.65,
        spd: Math.min(this.W, this.H) * 0.007,
      });
    },

    tick() {
      this.ctx.clearRect(0, 0, this.W, this.H);
      let dk = document.documentElement.classList.contains("dark");

      let dotColor = dk ? "rgba(113,113,122,0.75)" : "rgba(161,161,170,0.35)";

      // Draw grid dots
      for (let col = 0; col <= this.COLS; col++) {
        for (let row = 0; row <= this.ROWS; row++) {
          this.ctx.beginPath();
          this.ctx.arc(col * this.GX, row * this.GY, 1, 0, 6.2832);
          this.ctx.fillStyle = dotColor;
          this.ctx.fill();
        }
      }

      // Draw grid lines
      this.ctx.strokeStyle = dk
        ? "rgba(82,82,91,0.55)"
        : "rgba(212,212,216,0.65)";
      this.ctx.lineWidth = 0.5;
      for (let col = 0; col <= this.COLS; col++) {
        this.ctx.beginPath();
        this.ctx.moveTo(col * this.GX, 0);
        this.ctx.lineTo(col * this.GX, this.H);
        this.ctx.stroke();
      }
      for (let row = 0; row <= this.ROWS; row++) {
        this.ctx.beginPath();
        this.ctx.moveTo(0, row * this.GY);
        this.ctx.lineTo(this.W, row * this.GY);
        this.ctx.stroke();
      }

      this.frame++;
      if (this.frame % 50 === 0 && this.strikes.length < 22) this.addStrike();
      if (this.frame % 200 === 0) this.addPulse();

      // Handle pulses
      for (let pi = this.pulses.length - 1; pi >= 0; pi--) {
        let pu = this.pulses[pi];
        pu.r += pu.spd;
        if (pu.r > pu.maxR) {
          this.pulses.splice(pi, 1);
          continue;
        }
        let prog = pu.r / pu.maxR;
        let band = this.GX * 1.8;

        for (let col = 0; col <= this.COLS; col++) {
          for (let row = 0; row <= this.ROWS; row++) {
            let ndx = col * this.GX - pu.cx;
            let ndy = row * this.GY - pu.cy;
            let nd = Math.sqrt(ndx * ndx + ndy * ndy);
            let diff = Math.abs(nd - pu.r);

            if (diff < band) {
              let glow = (1 - diff / band) * (1 - prog) * 0.9;
              this.ctx.beginPath();
              this.ctx.arc(col * this.GX, row * this.GY, 2.5, 0, 6.2832);
              this.ctx.fillStyle = "rgba(52,211,153," + glow + ")";
              this.ctx.fill();
            }
          }
        }
      }

      // Handle strikes
      for (let i = this.strikes.length - 1; i >= 0; i--) {
        let s = this.strikes[i];
        s.pos += s.spd;

        if (s.h) {
          if (s.pos > this.COLS + s.len) {
            this.strikes.splice(i, 1);
            continue;
          }
          let gy = s.row * this.GY;
          for (let k = s.len; k >= 0; k--) {
            let tc = Math.round(s.pos) - k;
            if (tc < 0 || tc >= this.COLS) continue;
            let frac = 1 - k / s.len;
            let alpha = frac * 0.7;
            this.ctx.beginPath();
            this.ctx.moveTo(tc * this.GX, gy);
            this.ctx.lineTo((tc + 1) * this.GX, gy);
            this.ctx.strokeStyle = "rgba(52,211,153," + alpha * 0.8 + ")";
            this.ctx.lineWidth = k === 0 ? 2.2 : 1.4;
            this.ctx.stroke();
            this.ctx.beginPath();
            this.ctx.arc(tc * this.GX, gy, k === 0 ? 4 : 2, 0, 6.2832);
            this.ctx.fillStyle = "rgba(52,211,153," + alpha + ")";
            this.ctx.fill();
          }
        } else {
          if (s.pos > this.ROWS + s.len) {
            this.strikes.splice(i, 1);
            continue;
          }
          let ri = Math.round(s.pos);
          let gx = s.col * this.GX;
          for (let k = s.len; k >= 0; k--) {
            let tr = ri - k;
            if (tr < 0 || tr >= this.ROWS) continue;
            let frac = 1 - k / s.len;
            let alpha = frac * 0.7;
            this.ctx.beginPath();
            this.ctx.moveTo(gx, tr * this.GY);
            this.ctx.lineTo(gx, (tr + 1) * this.GY);
            this.ctx.strokeStyle = "rgba(52,211,153," + alpha * 0.8 + ")";
            this.ctx.lineWidth = k === 0 ? 2.2 : 1.4;
            this.ctx.stroke();
            this.ctx.beginPath();
            this.ctx.arc(gx, tr * this.GY, k === 0 ? 4 : 2, 0, 6.2832);
            this.ctx.fillStyle = "rgba(52,211,153," + alpha + ")";
            this.ctx.fill();
          }
        }
      }

      // Queue the next frame
      this.raf = requestAnimationFrame(() => this.tick());
    },
  }));
});
