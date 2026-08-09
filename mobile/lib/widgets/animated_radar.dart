import 'dart:math';
import 'package:flutter/material.dart';
import '../theme.dart';

class AnimatedRadar extends StatefulWidget {
  final bool isListening;

  const AnimatedRadar({super.key, required this.isListening});

  @override
  State<AnimatedRadar> createState() => _AnimatedRadarState();
}

class _AnimatedRadarState extends State<AnimatedRadar> with SingleTickerProviderStateMixin {
  late AnimationController _controller;

  @override
  void initState() {
    super.initState();
    _controller = AnimationController(
      vsync: this,
      duration: const Duration(seconds: 8),
    );
    if (widget.isListening) {
      _controller.repeat();
    }
  }

  @override
  void didUpdateWidget(AnimatedRadar oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.isListening && !oldWidget.isListening) {
      _controller.repeat();
    } else if (!widget.isListening && oldWidget.isListening) {
      _controller.stop();
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: 150,
      height: 150,
      child: Stack(
        alignment: Alignment.center,
        children: [
          // Segmented Ring
          AnimatedBuilder(
            animation: _controller,
            builder: (context, child) {
              return Transform.rotate(
                angle: _controller.value * 2 * pi,
                child: CustomPaint(
                  size: const Size(150, 150),
                  painter: _SegmentedRingPainter(
                    color: widget.isListening ? AppTheme.accentPrimary : AppTheme.borderColor,
                  ),
                ),
              );
            },
          ),
          // Core Circle
          Container(
            width: 40,
            height: 40,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: AppTheme.bgApp,
              border: Border.all(
                color: widget.isListening ? AppTheme.accentPrimary : AppTheme.borderColor,
                width: 2,
              ),
              boxShadow: widget.isListening
                  ? [
                      BoxShadow(
                        color: AppTheme.accentPrimary.withValues(alpha: 0.5),
                        blurRadius: 10,
                        spreadRadius: 2,
                      )
                    ]
                  : null,
            ),
            child: Icon(
              Icons.radar,
              size: 20,
              color: widget.isListening ? AppTheme.accentPrimary : AppTheme.borderColor,
            ),
          ),
        ],
      ),
    );
  }
}

class _SegmentedRingPainter extends CustomPainter {
  final Color color;

  _SegmentedRingPainter({required this.color});

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;

    final center = Offset(size.width / 2, size.height / 2);
    final radius = size.width / 2 - paint.strokeWidth;

    // Draw dashed circle
    const int segments = 12;
    const double sweepAngle = (2 * pi) / (segments * 2);

    for (int i = 0; i < segments; i++) {
      final startAngle = i * sweepAngle * 2;
      canvas.drawArc(
        Rect.fromCircle(center: center, radius: radius),
        startAngle,
        sweepAngle,
        false,
        paint,
      );
    }
  }

  @override
  bool shouldRepaint(_SegmentedRingPainter oldDelegate) => color != oldDelegate.color;
}
