import 'package:flutter/material.dart';
import 'package:mobile/src/rust/frb_generated.dart';

import 'screens/send_screen.dart';
import 'screens/receive_screen.dart';

import 'theme.dart';

import 'package:provider/provider.dart';
import 'services/settings_service.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();

  final settingsService = await SettingsService.init();

  runApp(
    ChangeNotifierProvider.value(
      value: settingsService,
      child: const PlenumApp(),
    ),
  );
}

class PlenumApp extends StatelessWidget {
  const PlenumApp({super.key});

  @override
  Widget build(BuildContext context) {
    return Consumer<SettingsService>(
      builder: (context, settings, _) {
        return MaterialApp(
          title: 'Plenum',
          theme: AppTheme.darkTheme, // TODO: add lightTheme if needed
          darkTheme: AppTheme.darkTheme,
          themeMode: settings.themeMode,
          home: const MainScreen(),
        );
      },
    );
  }
}

class MainScreen extends StatefulWidget {
  const MainScreen({super.key});

  @override
  State<MainScreen> createState() => _MainScreenState();
}

class _MainScreenState extends State<MainScreen> {
  int _selectedIndex = 0;

  static const List<Widget> _screens = <Widget>[
    SendScreen(),
    ReceiveScreen(),
  ];

  void _onItemTapped(int index) {
    setState(() {
      _selectedIndex = index;
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: IndexedStack(
        index: _selectedIndex,
        children: _screens,
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _selectedIndex,
        onDestinationSelected: _onItemTapped,
        destinations: const <NavigationDestination>[
          NavigationDestination(
            icon: Icon(Icons.send_outlined),
            selectedIcon: Icon(Icons.send),
            label: 'Send',
          ),
          NavigationDestination(
            icon: Icon(Icons.download_outlined),
            selectedIcon: Icon(Icons.download),
            label: 'Receive',
          ),
        ],
      ),
    );
  }
}
