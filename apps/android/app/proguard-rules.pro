# BeMyVPN — правила R8 для release-сборки.
#
# JNI-мост: нативная библиотека ищет методы по именам
# Java_org_bemyvpn_Native_nativeXxx — класс и имена native-методов должны
# пережить минификацию как есть.
-keepclasseswithmembernames class * {
    native <methods>;
}

# Компоненты из манифеста (Activity/Service) R8 сохраняет сам.
# Рефлексии в приложении нет, остальное можно резать смело.

# ZXing: генерация QR (QRCodeWriter) + встроенный сканер камеры
# (com.journeyapps.barcodescanner). Прячем предупреждения о необязательных
# классах (Android/js) — их у нас нет и не нужно.
-dontwarn com.google.zxing.**
# CaptureActivity сканера объявлена в манифесте библиотеки и запускается по классу —
# на всякий случай оставляем пакет сканера нетронутым для R8.
-keep class com.journeyapps.barcodescanner.** { *; }
