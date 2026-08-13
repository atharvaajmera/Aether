import 'package:flutter/material.dart';
import 'package:google_fonts/google_fonts.dart';

class AppTheme {
  // Desktop Palette
  static const Color bgApp = Color(0xFF181F2A);
  static const Color bgSidebar = Color(0xFF2C3949);
  static const Color bgCard = Color(0xFF222C3A);
  static const Color bgCardHover = Color(0xFF2A3748);

  static const Color textPrimary = Color(0xFFF8FAFC);
  static const Color textSecondary = Color(0xFF94A3B8);

  static const Color accentPrimary = Color(0xFF59B286);
  static const Color accentPill = Color(0xFF290E7F);
  static const Color borderColor = Color(0xFF3B4B5E);

  // Light Palette
  static const Color bgAppLight = Color(0xFFF1F5F9);
  static const Color bgSidebarLight = Color(0xFFE2E8F0);
  static const Color bgCardLight = Color(0xFFFFFFFF);
  static const Color bgCardHoverLight = Color(0xFFF8FAFC);

  static const Color textPrimaryLight = Color(0xFF0F172A);
  static const Color textSecondaryLight = Color(0xFF64748B);

  static const Color borderColorLight = Color(0xFFCBD5E1);

  static ThemeData get darkTheme {
    return ThemeData(
      useMaterial3: true,
      brightness: Brightness.dark,
      scaffoldBackgroundColor: bgApp,
      colorScheme: const ColorScheme.dark(
        primary: accentPrimary,
        secondary: accentPill,
        surface: bgCard,
        onSurface: textPrimary,
      ),
      textTheme: GoogleFonts.interTextTheme(
        ThemeData.dark().textTheme.copyWith(
              bodyLarge: const TextStyle(color: textPrimary),
              bodyMedium: const TextStyle(color: textPrimary),
              bodySmall: const TextStyle(color: textSecondary),
              titleLarge: const TextStyle(color: textPrimary, fontWeight: FontWeight.bold),
              titleMedium: const TextStyle(color: textPrimary, fontWeight: FontWeight.w600),
            ),
      ),
      appBarTheme: const AppBarTheme(
        backgroundColor: Colors.transparent,
        elevation: 0,
        centerTitle: false,
        iconTheme: IconThemeData(color: textPrimary),
        titleTextStyle: TextStyle(
          color: textPrimary,
          fontSize: 20,
          fontWeight: FontWeight.bold,
        ),
      ),
      navigationBarTheme: NavigationBarThemeData(
        backgroundColor: bgSidebar,
        indicatorColor: accentPrimary.withValues(alpha: 0.2),
        labelTextStyle: WidgetStateProperty.resolveWith((states) {
          if (states.contains(WidgetState.selected)) {
            return const TextStyle(color: accentPrimary, fontSize: 12, fontWeight: FontWeight.w600);
          }
          return const TextStyle(color: textSecondary, fontSize: 12);
        }),
        iconTheme: WidgetStateProperty.resolveWith((states) {
          if (states.contains(WidgetState.selected)) {
            return const IconThemeData(color: accentPrimary);
          }
          return const IconThemeData(color: textSecondary);
        }),
      ),
      cardTheme: CardThemeData(
        color: bgCard,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(16),
          side: const BorderSide(color: borderColor, width: 1),
        ),
        elevation: 0,
      ),
      elevatedButtonTheme: ElevatedButtonThemeData(
        style: ElevatedButton.styleFrom(
          backgroundColor: accentPrimary,
          foregroundColor: bgApp,
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
          padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 12),
          textStyle: const TextStyle(fontWeight: FontWeight.w600),
        ),
      ),
      iconTheme: const IconThemeData(color: textSecondary),
    );
  }

  static ThemeData get lightTheme {
    return ThemeData(
      useMaterial3: true,
      brightness: Brightness.light,
      scaffoldBackgroundColor: bgAppLight,
      colorScheme: const ColorScheme.light(
        primary: accentPrimary,
        secondary: accentPill,
        surface: bgCardLight,
        onSurface: textPrimaryLight,
      ),
      textTheme: GoogleFonts.interTextTheme(
        ThemeData.light().textTheme.copyWith(
              bodyLarge: const TextStyle(color: textPrimaryLight),
              bodyMedium: const TextStyle(color: textPrimaryLight),
              bodySmall: const TextStyle(color: textSecondaryLight),
              titleLarge: const TextStyle(color: textPrimaryLight, fontWeight: FontWeight.bold),
              titleMedium: const TextStyle(color: textPrimaryLight, fontWeight: FontWeight.w600),
            ),
      ),
      appBarTheme: const AppBarTheme(
        backgroundColor: Colors.transparent,
        elevation: 0,
        centerTitle: false,
        iconTheme: IconThemeData(color: textPrimaryLight),
        titleTextStyle: TextStyle(
          color: textPrimaryLight,
          fontSize: 20,
          fontWeight: FontWeight.bold,
        ),
      ),
      navigationBarTheme: NavigationBarThemeData(
        backgroundColor: bgSidebarLight,
        indicatorColor: accentPrimary.withValues(alpha: 0.2),
        labelTextStyle: WidgetStateProperty.resolveWith((states) {
          if (states.contains(WidgetState.selected)) {
            return const TextStyle(color: accentPrimary, fontSize: 12, fontWeight: FontWeight.w600);
          }
          return const TextStyle(color: textSecondaryLight, fontSize: 12);
        }),
        iconTheme: WidgetStateProperty.resolveWith((states) {
          if (states.contains(WidgetState.selected)) {
            return const IconThemeData(color: accentPrimary);
          }
          return const IconThemeData(color: textSecondaryLight);
        }),
      ),
      cardTheme: CardThemeData(
        color: bgCardLight,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(16),
          side: const BorderSide(color: borderColorLight, width: 1),
        ),
        elevation: 0,
      ),
      elevatedButtonTheme: ElevatedButtonThemeData(
        style: ElevatedButton.styleFrom(
          backgroundColor: accentPrimary,
          foregroundColor: bgAppLight,
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
          padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 12),
          textStyle: const TextStyle(fontWeight: FontWeight.w600),
        ),
      ),
      iconTheme: const IconThemeData(color: textSecondaryLight),
    );
  }
}
