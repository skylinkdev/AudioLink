import 'package:flutter_test/flutter_test.dart';

import 'package:lan_audio_flutter/main.dart';

void main() {
  testWidgets('shows Link audio controls', (tester) async {
    await tester.pumpWidget(const LanAudioApp());

    expect(find.text('Audio Link'), findsWidgets);
    expect(find.text('扫描'), findsOneWidget);
    expect(find.text('手动'), findsOneWidget);
    expect(find.text('设置'), findsOneWidget);
    expect(find.text('服务端'), findsOneWidget);
  });
}
