/* Cabecera envoltorio para bindgen: reune todo lo que necesitamos de libvpx en una sola
 * unidad de traduccion. No se incluyen vpx_ext_ratectrl.h ni vpx_tpl.h porque solo hacen
 * falta para el control de bitrate externo, que no usamos. */

#include <vpx/vpx_codec.h>
#include <vpx/vpx_image.h>
#include <vpx/vpx_encoder.h>
#include <vpx/vpx_decoder.h>
#include <vpx/vp8.h>
#include <vpx/vp8cx.h>
#include <vpx/vp8dx.h>
