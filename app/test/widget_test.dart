import 'package:daveaudioplayer/main.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('formatMs renders minutes and seconds', () {
    expect(formatMs(null), '–:––');
    expect(formatMs(0), '0:00');
    expect(formatMs(61_500), '1:01');
    expect(formatMs(3_600_000), '60:00');
  });
}
