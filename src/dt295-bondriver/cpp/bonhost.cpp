/*
 * bonhost.cpp - calls into a loaded BonDriver through its IBonDriver2 vtable, for the
 * bontest tool (src/bin/bontest.rs), which loads the DLL and drives it the way TVTest
 * does. The C++ compiler does the virtual dispatch, so the slots are the ones
 * IBonDriver2.h gives TVTest (including MSVC's order of the GetTsStream overloads).
 *
 * Its own object file: only bontest references it, so the linker never pulls it into
 * BonDriver_DT295.dll.
 */
#include "IBonDriver2.h"

typedef IBonDriver *(*CreateBonDriverFn)();

/* CreateBonDriver(), then dynamic_cast to IBonDriver2 as TVTest does; NULL (and the
 * driver released) if it is not one. */
extern "C" IBonDriver2 *bonhost_create(CreateBonDriverFn create) noexcept
{
    IBonDriver *b = create ? create() : nullptr;
    IBonDriver2 *b2 = b ? dynamic_cast<IBonDriver2 *>(b) : nullptr;
    if (b && !b2) b->Release();
    return b2;
}

extern "C" LPCWSTR bonhost_tuner_name(IBonDriver2 *b) noexcept { return b->GetTunerName(); }
extern "C" LPCWSTR bonhost_enum_space(IBonDriver2 *b, DWORD s) noexcept { return b->EnumTuningSpace(s); }
extern "C" LPCWSTR bonhost_enum_channel(IBonDriver2 *b, DWORD s, DWORD c) noexcept
{
    return b->EnumChannelName(s, c);
}
extern "C" BOOL bonhost_open(IBonDriver2 *b) noexcept { return b->OpenTuner(); }
extern "C" BOOL bonhost_is_opening(IBonDriver2 *b) noexcept { return b->IsTunerOpening(); }
extern "C" BOOL bonhost_set_channel(IBonDriver2 *b, DWORD s, DWORD c) noexcept { return b->SetChannel(s, c); }
extern "C" DWORD bonhost_cur_space(IBonDriver2 *b) noexcept { return b->GetCurSpace(); }
extern "C" DWORD bonhost_cur_channel(IBonDriver2 *b) noexcept { return b->GetCurChannel(); }
extern "C" float bonhost_signal(IBonDriver2 *b) noexcept { return b->GetSignalLevel(); }
extern "C" DWORD bonhost_wait(IBonDriver2 *b, DWORD ms) noexcept { return b->WaitTsStream(ms); }
/* The BYTE ** overload (the driver's buffer), as TVTest uses it. */
extern "C" BOOL bonhost_get_ts(IBonDriver2 *b, BYTE **p, DWORD *size, DWORD *remain) noexcept
{
    return b->GetTsStream(p, size, remain);
}
extern "C" void bonhost_close(IBonDriver2 *b) noexcept { b->CloseTuner(); }
extern "C" void bonhost_release(IBonDriver2 *b) noexcept { b->Release(); }
