# Scoreboard Ctrl

Side project to add web a interface to a GameCraft [SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI?th=1) Scoreboard. Why? 
1. Add ability to reset timer (missing from included remote).
2. Get familiar with [embassy-rs](https://github.com/embassy-rs) and [picoserve](https://github.com/sammhicks/picoserve).
3. Play with [OpenWrt](https://openwrt.org/).

## Hardware

- [GameCraft SK2229R](https://www.amazon.com/BSN-Multisport-Indoor-Tabletop-Scoreboard/dp/B003SFP4CI?th=1) Scoreboard
- [GL-MT300N-V2](https://www.gl-inet.com/products/gl-mt300n-v2/) Router
- [Raspberry Pi Pico](https://www.raspberrypi.org/products/raspberry-pi-pico/) with [PicoW](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html#raspberry-pi-pico-w) board

- [CD4066BE](https://www.ti.com/lit/ds/symlink/cd4066b.pdf) analog switch or equivalent (it's what's in the GameCraft scoreboard)
