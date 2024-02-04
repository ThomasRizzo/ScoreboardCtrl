# Scoreboard Ctrl

Adds web a interface to a GameCraft [SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI?th=1) Scoreboard. Why?

1. Add ability to reset timer (missing from included remote).
2. Get familiar with [embassy-rs](https://github.com/embassy-rs) and [picoserve](https://github.com/sammhicks/picoserve).
3. Play with [OpenWrt](https://openwrt.org/).

## Hardware

- [GameCraft SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI?th=1) Scoreboard
- [GL-MT300N-V2](https://www.gl-inet.com/products/gl-mt300n-v2/) Router
- [Pi Pico W](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html#raspberry-pi-pico-w) for pressing buttons on the scoreboard and reading time remaining via web interface.
- [CD74HCT4066](https://www.ti.com/lit/ds/symlink/cd74hct4066.pdf) analog switch for controlling buttons (Scoreboard uses 5v CD4066BE)
- [MAX3232](https://www.ti.com/lit/ds/symlink/max3232.pdf) for scoreboard RS232 to pipico TTL.