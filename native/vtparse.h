#ifndef OPENTERM_VTPARSE_H
#define OPENTERM_VTPARSE_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct vt_parser vt_parser;

typedef void (*vt_cb_print)   (void *ud, uint32_t codepoint);
typedef void (*vt_cb_execute) (void *ud, uint8_t c0);
typedef void (*vt_cb_csi)     (void *ud, uint8_t final, const uint32_t *params, uint8_t n_params,
                               const uint8_t *subparams, const uint8_t *intermediates,
                               uint8_t n_intermediates);
typedef void (*vt_cb_esc)     (void *ud, uint8_t final, const uint8_t *intermediates, uint8_t n_intermediates);
typedef void (*vt_cb_osc)     (void *ud, const uint8_t *data, uint32_t len);
typedef void (*vt_cb_dcs)     (void *ud, uint8_t final, const uint32_t *params, uint8_t n_params,
                               const uint8_t *subparams, const uint8_t *intermediates,
                               uint8_t n_intermediates, const uint8_t *data, uint32_t len);

size_t vt_sizeof(void);
void   vt_init(vt_parser *p);
void   vt_set_callbacks(vt_parser *p, vt_cb_print, vt_cb_execute, vt_cb_csi, vt_cb_esc,
                        vt_cb_osc, vt_cb_dcs, void *userdata);
void   vt_feed(vt_parser *p, const uint8_t *data, size_t len);

#ifdef __cplusplus
}
#endif
#endif
