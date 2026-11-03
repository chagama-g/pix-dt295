/*
 * bonshim.cpp - the IBonDriver2 object TVTest sees, forwarding to the Rust driver.
 *
 * TVTest calls the BonDriver through a C++ vtable and dynamic_cast<IBonDriver2 *>s
 * the pointer CreateBonDriver returns, so the object must be a real MSVC class with
 * MSVC RTTI (and __thiscall methods on x86). CBonShim is that class: each method
 * forwards to a plain C function pointer of BonOps (the Rust side, see src/ffi.rs)
 * with the opaque Rust driver pointer as the first argument. No logic lives here.
 *
 * BonOps must match `struct BonOps` in src/ffi.rs field for field.
 */
#include <new>
#include "IBonDriver2.h"

#define BONOPS_CALL __cdecl   /* = Rust extern "C" on x86; ignored on x64 */

extern "C" {

struct BonOps {
    /* IBonDriver */
    BOOL    (BONOPS_CALL *open_tuner)(void *ctx);
    void    (BONOPS_CALL *close_tuner)(void *ctx);
    BOOL    (BONOPS_CALL *set_channel_byte)(void *ctx, BYTE ch);
    float   (BONOPS_CALL *get_signal_level)(void *ctx);
    DWORD   (BONOPS_CALL *wait_ts_stream)(void *ctx, DWORD timeout_ms);
    DWORD   (BONOPS_CALL *get_ready_count)(void *ctx);
    BOOL    (BONOPS_CALL *get_ts_stream_copy)(void *ctx, BYTE *dst, DWORD *size, DWORD *remain);
    BOOL    (BONOPS_CALL *get_ts_stream_ptr)(void *ctx, BYTE **dst, DWORD *size, DWORD *remain);
    void    (BONOPS_CALL *purge_ts_stream)(void *ctx);
    void    (BONOPS_CALL *release)(void *ctx);
    /* IBonDriver2 */
    LPCWSTR (BONOPS_CALL *get_tuner_name)(void *ctx);
    BOOL    (BONOPS_CALL *is_tuner_opening)(void *ctx);
    LPCWSTR (BONOPS_CALL *enum_tuning_space)(void *ctx, DWORD space);
    LPCWSTR (BONOPS_CALL *enum_channel_name)(void *ctx, DWORD space, DWORD channel);
    BOOL    (BONOPS_CALL *set_channel)(void *ctx, DWORD space, DWORD channel);
    DWORD   (BONOPS_CALL *get_cur_space)(void *ctx);
    DWORD   (BONOPS_CALL *get_cur_channel)(void *ctx);
};

}  /* extern "C" */

static_assert(sizeof(BonOps) == 17 * sizeof(void *), "BonOps must match src/ffi.rs");

class CBonShim final : public IBonDriver2
{
public:
    CBonShim(const BonOps *ops, void *driver) noexcept : o(ops), ctx(driver) {}

    /* IBonDriver */
    const BOOL  OpenTuner(void) override { return o->open_tuner(ctx); }
    void        CloseTuner(void) override { o->close_tuner(ctx); }
    const BOOL  SetChannel(const BYTE bCh) override { return o->set_channel_byte(ctx, bCh); }
    const float GetSignalLevel(void) override { return o->get_signal_level(ctx); }
    const DWORD WaitTsStream(const DWORD dwTimeOut = 0) override { return o->wait_ts_stream(ctx, dwTimeOut); }
    const DWORD GetReadyCount(void) override { return o->get_ready_count(ctx); }
    const BOOL  GetTsStream(BYTE *pDst, DWORD *pdwSize, DWORD *pdwRemain) override
    {
        return o->get_ts_stream_copy(ctx, pDst, pdwSize, pdwRemain);
    }
    const BOOL  GetTsStream(BYTE **ppDst, DWORD *pdwSize, DWORD *pdwRemain) override
    {
        return o->get_ts_stream_ptr(ctx, ppDst, pdwSize, pdwRemain);
    }
    void        PurgeTsStream(void) override { o->purge_ts_stream(ctx); }

    /* IBonDriver2 */
    LPCWSTR     GetTunerName(void) override { return o->get_tuner_name(ctx); }
    const BOOL  IsTunerOpening(void) override { return o->is_tuner_opening(ctx); }
    LPCWSTR     EnumTuningSpace(const DWORD dwSpace) override { return o->enum_tuning_space(ctx, dwSpace); }
    LPCWSTR     EnumChannelName(const DWORD dwSpace, const DWORD dwChannel) override
    {
        return o->enum_channel_name(ctx, dwSpace, dwChannel);
    }
    const BOOL  SetChannel(const DWORD dwSpace, const DWORD dwChannel) override
    {
        return o->set_channel(ctx, dwSpace, dwChannel);
    }
    const DWORD GetCurSpace(void) override { return o->get_cur_space(ctx); }
    const DWORD GetCurChannel(void) override { return o->get_cur_channel(ctx); }

    /* The Rust driver is gone after release(); then this object goes too. */
    void        Release(void) override
    {
        o->release(ctx);
        delete this;
    }

private:
    const BonOps *o;
    void *ctx;
};

/* Called by the Rust CreateBonDriver. NULL if out of memory. */
extern "C" IBonDriver *bonshim_new(const BonOps *ops, void *ctx) noexcept
{
    return new (std::nothrow) CBonShim(ops, ctx);
}
