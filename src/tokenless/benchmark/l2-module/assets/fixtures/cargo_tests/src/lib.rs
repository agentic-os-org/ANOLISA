//! L2 fixture: forty unit tests; `L2_FIXTURE_FAIL=1` fails `case_27`.

pub fn square(x: u64) -> u64 {
    x * x
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn case_01() {
        assert_eq!(square(1), 1);
    }


    #[test]
    fn case_02() {
        assert_eq!(square(2), 4);
    }


    #[test]
    fn case_03() {
        assert_eq!(square(3), 9);
    }


    #[test]
    fn case_04() {
        assert_eq!(square(4), 16);
    }


    #[test]
    fn case_05() {
        assert_eq!(square(5), 25);
    }


    #[test]
    fn case_06() {
        assert_eq!(square(6), 36);
    }


    #[test]
    fn case_07() {
        assert_eq!(square(7), 49);
    }


    #[test]
    fn case_08() {
        assert_eq!(square(8), 64);
    }


    #[test]
    fn case_09() {
        assert_eq!(square(9), 81);
    }


    #[test]
    fn case_10() {
        assert_eq!(square(10), 100);
    }


    #[test]
    fn case_11() {
        assert_eq!(square(11), 121);
    }


    #[test]
    fn case_12() {
        assert_eq!(square(12), 144);
    }


    #[test]
    fn case_13() {
        assert_eq!(square(13), 169);
    }


    #[test]
    fn case_14() {
        assert_eq!(square(14), 196);
    }


    #[test]
    fn case_15() {
        assert_eq!(square(15), 225);
    }


    #[test]
    fn case_16() {
        assert_eq!(square(16), 256);
    }


    #[test]
    fn case_17() {
        assert_eq!(square(17), 289);
    }


    #[test]
    fn case_18() {
        assert_eq!(square(18), 324);
    }


    #[test]
    fn case_19() {
        assert_eq!(square(19), 361);
    }


    #[test]
    fn case_20() {
        assert_eq!(square(20), 400);
    }


    #[test]
    fn case_21() {
        assert_eq!(square(21), 441);
    }


    #[test]
    fn case_22() {
        assert_eq!(square(22), 484);
    }


    #[test]
    fn case_23() {
        assert_eq!(square(23), 529);
    }


    #[test]
    fn case_24() {
        assert_eq!(square(24), 576);
    }


    #[test]
    fn case_25() {
        assert_eq!(square(25), 625);
    }


    #[test]
    fn case_26() {
        assert_eq!(square(26), 676);
    }


    #[test]
    fn case_27() {
        let expected = if std::env::var_os("L2_FIXTURE_FAIL").is_some() { 730 } else { 729 };
        assert_eq!(square(27), expected, "square(27) diverged from the reference table");
    }


    #[test]
    fn case_28() {
        assert_eq!(square(28), 784);
    }


    #[test]
    fn case_29() {
        assert_eq!(square(29), 841);
    }


    #[test]
    fn case_30() {
        assert_eq!(square(30), 900);
    }


    #[test]
    fn case_31() {
        assert_eq!(square(31), 961);
    }


    #[test]
    fn case_32() {
        assert_eq!(square(32), 1024);
    }


    #[test]
    fn case_33() {
        assert_eq!(square(33), 1089);
    }


    #[test]
    fn case_34() {
        assert_eq!(square(34), 1156);
    }


    #[test]
    fn case_35() {
        assert_eq!(square(35), 1225);
    }


    #[test]
    fn case_36() {
        assert_eq!(square(36), 1296);
    }


    #[test]
    fn case_37() {
        assert_eq!(square(37), 1369);
    }


    #[test]
    fn case_38() {
        assert_eq!(square(38), 1444);
    }


    #[test]
    fn case_39() {
        assert_eq!(square(39), 1521);
    }


    #[test]
    fn case_40() {
        assert_eq!(square(40), 1600);
    }

}
