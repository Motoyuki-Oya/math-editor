// リリースでは Windows 上で追加のコンソール ウィンドウが表示されないようにします。削除しないでください。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    planetext_lib::run()
}
