use bombechat_core::{safety_number, Identity};

#[test]
fn safety_number_is_symmetric() {
    let (a, b) = (Identity::generate().public(), Identity::generate().public());
    assert_eq!(safety_number(&a, &b), safety_number(&b, &a));
}

#[test]
fn safety_number_is_60_digits_in_12_groups() {
    let s = safety_number(
        &Identity::generate().public(),
        &Identity::generate().public(),
    );
    let g: Vec<&str> = s.split(' ').collect();
    assert_eq!(g.len(), 12);
    assert!(g
        .iter()
        .all(|x| x.len() == 5 && x.chars().all(|c| c.is_ascii_digit())));
}

#[test]
fn one_bit_changes_the_number() {
    let a = Identity::generate().public();
    let mut b = Identity::generate().public();
    let s1 = safety_number(&a, &b);
    b.dh[0] ^= 1;
    assert_ne!(s1, safety_number(&a, &b));
}

#[test]
fn secret_roundtrip() {
    let i = Identity::generate();
    assert_eq!(
        Identity::from_secret_bytes(&i.secret_bytes()).public(),
        i.public()
    );
}
