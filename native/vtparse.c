/* vtparse.c — Paul Williams VT100/xterm state-machine parser.
 * Hot path: called per byte from the pty read loop. Zero heap allocations.
 */
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include "vtparse.h"

#define VT_MAX_PARAMS 16
#define VT_MAX_INTERMEDIATE 2
#define VT_STRING_BUF (64u * 1024u)

typedef enum {
    S_GROUND = 0, S_ESC, S_ESC_INT,
    S_CSI_ENTRY, S_CSI_PARAM, S_CSI_INT, S_CSI_IGNORE,
    S_OSC_STRING, S_OSC_ESC,
    S_DCS_ENTRY, S_DCS_PASS, S_DCS_ESC, S_DCS_IGNORE, S_DCS_IGNORE_ESC,
    S_SOS_PM_APC, S_SOS_PM_APC_ESC
} vt_state_t;

struct vt_parser {
    vt_state_t state;
    uint32_t params[VT_MAX_PARAMS];
    uint8_t  subparams[VT_MAX_PARAMS];
    uint8_t  n_params;
    uint8_t  intermediate[VT_MAX_INTERMEDIATE];
    uint8_t  n_intermediate;
    uint8_t  string_buf[VT_STRING_BUF];
    uint32_t string_len;
    uint8_t  dcs_final;
    uint32_t utf8_cp;
    uint32_t utf8_min;
    uint8_t  utf8_need;

    vt_cb_print    cb_print;
    vt_cb_execute  cb_execute;
    vt_cb_csi      cb_csi;
    vt_cb_esc      cb_esc;
    vt_cb_osc      cb_osc;
    vt_cb_dcs      cb_dcs;
    void          *userdata;
};

size_t vt_sizeof(void) { return sizeof(struct vt_parser); }

void vt_init(struct vt_parser *p) {
    memset(p, 0, sizeof(*p));
    p->state = S_GROUND;
}

void vt_set_callbacks(struct vt_parser *p,
                      vt_cb_print print_cb,
                      vt_cb_execute exec_cb,
                      vt_cb_csi csi_cb,
                      vt_cb_esc esc_cb,
                      vt_cb_osc osc_cb,
                      vt_cb_dcs dcs_cb,
                      void *userdata) {
    p->cb_print   = print_cb;
    p->cb_execute = exec_cb;
    p->cb_csi     = csi_cb;
    p->cb_esc     = esc_cb;
    p->cb_osc     = osc_cb;
    p->cb_dcs     = dcs_cb;
    p->userdata   = userdata;
}

static inline int push_param(struct vt_parser *p, uint8_t b) {
    if (p->n_params == 0) p->n_params = 1;
    if (p->n_params > VT_MAX_PARAMS) return 0;
    uint32_t *cur = &p->params[p->n_params - 1];
    if (b == ';' || b == ':') {
        if (p->n_params >= VT_MAX_PARAMS) return 0;
        uint8_t next = p->n_params;
        p->n_params++;
        p->params[p->n_params - 1] = 0;
        p->subparams[next] = (b == ':');
    } else {
        uint64_t v = (uint64_t)(*cur) * 10 + (uint64_t)(b - '0');
        if (v > 0xFFFFu) v = 0xFFFF;
        *cur = (uint32_t)v;
    }
    return 1;
}

static inline void clear_params(struct vt_parser *p) {
    p->n_params = 0;
    p->n_intermediate = 0;
    memset(p->params, 0, sizeof(p->params));
    memset(p->subparams, 0, sizeof(p->subparams));
}

static inline void append_string(struct vt_parser *p, uint8_t b) {
    if (p->string_len < VT_STRING_BUF) p->string_buf[p->string_len++] = b;
}

static inline void finish_osc(struct vt_parser *p) {
    if (p->cb_osc) p->cb_osc(p->userdata, p->string_buf, p->string_len);
    p->state = S_GROUND;
}

static inline void finish_dcs(struct vt_parser *p) {
    if (p->cb_dcs) p->cb_dcs(p->userdata, p->dcs_final, p->params, p->n_params,
                             p->subparams, p->intermediate, p->n_intermediate,
                             p->string_buf, p->string_len);
    p->state = S_GROUND;
}

void vt_feed(struct vt_parser *p, const uint8_t *data, size_t len) {
    for (size_t i = 0; i < len; i++) {
        uint8_t b = data[i];

        if (b == 0x18 || b == 0x1A) { p->state = S_GROUND; continue; }

        /* OSC, DCS and ignored string states use ESC \\ (ST). Keep the ESC
         * until the following byte tells us whether it really was ST. */
        if (p->state == S_OSC_STRING) {
            if (b == 0x07 || b == 0x9C) finish_osc(p);
            else if (b == 0x1B) p->state = S_OSC_ESC;
            else append_string(p, b);
            continue;
        }
        if (p->state == S_OSC_ESC) {
            if (b == '\\') finish_osc(p);
            else if (b == 0x07 || b == 0x9C) {
                append_string(p, 0x1B);
                finish_osc(p);
            }
            else {
                append_string(p, 0x1B);
                append_string(p, b);
                p->state = S_OSC_STRING;
            }
            continue;
        }
        if (p->state == S_DCS_PASS) {
            if (b == 0x9C) finish_dcs(p);
            else if (b == 0x1B) p->state = S_DCS_ESC;
            else append_string(p, b);
            continue;
        }
        if (p->state == S_DCS_ESC) {
            if (b == '\\') finish_dcs(p);
            else {
                append_string(p, 0x1B);
                append_string(p, b);
                p->state = S_DCS_PASS;
            }
            continue;
        }
        if (p->state == S_DCS_IGNORE) {
            if (b == 0x9C) p->state = S_GROUND;
            else if (b == 0x1B) p->state = S_DCS_IGNORE_ESC;
            continue;
        }
        if (p->state == S_DCS_IGNORE_ESC) {
            p->state = (b == '\\') ? S_GROUND : S_DCS_IGNORE;
            continue;
        }
        if (p->state == S_SOS_PM_APC) {
            if (b == 0x9C) p->state = S_GROUND;
            else if (b == 0x1B) p->state = S_SOS_PM_APC_ESC;
            continue;
        }
        if (p->state == S_SOS_PM_APC_ESC) {
            p->state = (b == '\\') ? S_GROUND : S_SOS_PM_APC;
            continue;
        }

        /* mid-sequence utf-8 interrupted by a non-continuation byte: emit U+FFFD */
        if (p->utf8_need && (b & 0xC0) != 0x80) {
            p->utf8_need = 0;
            p->utf8_min = 0;
            if (p->cb_print) p->cb_print(p->userdata, 0xFFFD);
        }

        if (b == 0x1B) { clear_params(p); p->state = S_ESC; continue; }

        switch (p->state) {
        case S_GROUND:
            if (b >= 0x80) {
                if (p->utf8_need) {                       /* continuation */
                    p->utf8_cp = (p->utf8_cp << 6) | (b & 0x3F);
                    if (--p->utf8_need == 0) {
                        uint32_t cp = p->utf8_cp;
                        if (cp < p->utf8_min || (cp >= 0xD800 && cp <= 0xDFFF) || cp > 0x10FFFF)
                            cp = 0xFFFD;
                        p->utf8_min = 0;
                        if (p->cb_print) p->cb_print(p->userdata, cp);
                    }
                } else if (b >= 0xC2 && b <= 0xDF) {
                    p->utf8_cp = b & 0x1F; p->utf8_min = 0x80; p->utf8_need = 1;
                }
                else if (b >= 0xE0 && b <= 0xEF) {
                    p->utf8_cp = b & 0x0F; p->utf8_min = 0x800; p->utf8_need = 2;
                }
                else if (b >= 0xF0 && b <= 0xF4) {
                    p->utf8_cp = b & 0x07; p->utf8_min = 0x10000; p->utf8_need = 3;
                }
                else if (p->cb_print) p->cb_print(p->userdata, 0xFFFD); /* stray continuation */
            } else if (b < 0x20 || b == 0x7F) {
                if (p->cb_execute) p->cb_execute(p->userdata, b);
            } else {
                if (p->cb_print) p->cb_print(p->userdata, (uint32_t)b);
            }
            break;

        case S_ESC:
            if (b == '[')      { clear_params(p); p->state = S_CSI_ENTRY; }
            else if (b == ']') { p->string_len = 0; p->state = S_OSC_STRING; }
            else if (b == 'P') { clear_params(p); p->state = S_DCS_ENTRY; }
            else if (b == 'X' || b == '^' || b == '_') { p->state = S_SOS_PM_APC; }
            else if (b >= 0x20 && b <= 0x2F) {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
                p->state = S_ESC_INT;
            } else if (b >= 0x30 && b <= 0x7E) {
                if (p->cb_esc) p->cb_esc(p->userdata, b, p->intermediate, p->n_intermediate);
                p->state = S_GROUND;
            }
            break;

        case S_ESC_INT:
            if (b >= 0x20 && b <= 0x2F) {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
            } else if (b >= 0x30 && b <= 0x7E) {
                if (p->cb_esc) p->cb_esc(p->userdata, b, p->intermediate, p->n_intermediate);
                p->state = S_GROUND;
            }
            break;

        case S_CSI_ENTRY:
            if ((b >= '0' && b <= '9') || b == ';' || b == ':') {
                p->state = push_param(p, b) ? S_CSI_PARAM : S_CSI_IGNORE;
            }
            else if (b == '?' || b == '>' || b == '<' || b == '=') {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
                else p->state = S_CSI_IGNORE;
                if (p->state != S_CSI_IGNORE) p->state = S_CSI_PARAM;
            } else if (b >= 0x20 && b <= 0x2F) {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
                else p->state = S_CSI_IGNORE;
                if (p->state != S_CSI_IGNORE) p->state = S_CSI_INT;
            } else if (b >= 0x40 && b <= 0x7E) {
                if (p->cb_csi) p->cb_csi(p->userdata, b, p->params, p->n_params,
                                         p->subparams, p->intermediate, p->n_intermediate);
                p->state = S_GROUND;
            }
            break;

        case S_CSI_PARAM:
            if ((b >= '0' && b <= '9') || b == ';' || b == ':') {
                if (!push_param(p, b)) p->state = S_CSI_IGNORE;
            }
            else if (b >= 0x3C && b <= 0x3F) p->state = S_CSI_IGNORE;
            else if (b >= 0x20 && b <= 0x2F) {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
                else p->state = S_CSI_IGNORE;
                if (p->state != S_CSI_IGNORE) p->state = S_CSI_INT;
            } else if (b >= 0x40 && b <= 0x7E) {
                if (p->cb_csi) p->cb_csi(p->userdata, b, p->params, p->n_params,
                                         p->subparams, p->intermediate, p->n_intermediate);
                p->state = S_GROUND;
            }
            break;

        case S_CSI_INT:
            if (b >= 0x30 && b <= 0x3F) p->state = S_CSI_IGNORE;
            else if (b >= 0x20 && b <= 0x2F) {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
                else p->state = S_CSI_IGNORE;
            } else if (b >= 0x40 && b <= 0x7E) {
                if (p->cb_csi) p->cb_csi(p->userdata, b, p->params, p->n_params,
                                         p->subparams, p->intermediate, p->n_intermediate);
                p->state = S_GROUND;
            }
            break;

        case S_CSI_IGNORE:
            if (b >= 0x40 && b <= 0x7E) p->state = S_GROUND;
            break;

        case S_DCS_ENTRY:
            if ((b >= '0' && b <= '9') || b == ';' || b == ':') {
                if (!push_param(p, b)) p->state = S_DCS_IGNORE;
            }
            else if (b >= 0x3C && b <= 0x3F) p->state = S_DCS_IGNORE;
            else if (b >= 0x20 && b <= 0x2F) {
                if (p->n_intermediate < VT_MAX_INTERMEDIATE)
                    p->intermediate[p->n_intermediate++] = b;
                else p->state = S_DCS_IGNORE;
            } else if (b >= 0x40 && b <= 0x7E) {
                p->dcs_final = b;
                p->string_len = 0;
                p->state = S_DCS_PASS;
            }
            break;
        case S_DCS_IGNORE:
        case S_DCS_IGNORE_ESC:
        case S_OSC_STRING:
        case S_OSC_ESC:
        case S_DCS_PASS:
        case S_DCS_ESC:
        case S_SOS_PM_APC:
        case S_SOS_PM_APC_ESC:
            /* handled before the global ESC transition above */
            break;
        }
    }
}
