| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |
|---|---|---|---|---|---|---|---|---|---|
| `gcd` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / nat-result (≥ 0)=SOUND | SOUND | SOUND | commutative=EXHAUSTIVE; right_zero=EXHAUSTIVE; divides_both=EXHAUSTIVE; associative=EXHAUSTIVE | commutative=PROVED; right_zero=PROVED; divides_both=UNKNOWN; associative=UNKNOWN |
| `lcm` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=SOUND | SOUND | SOUND | multiple_of_both=EXHAUSTIVE; commutative=EXHAUSTIVE | multiple_of_both=UNKNOWN; commutative=PROVED |
| `ext_gcd` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | — | — |
| `bezout_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | bezout_identity=EXHAUSTIVE | bezout_identity=UNKNOWN |
| `mod_inverse` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `inverse_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | inverse_identity=EXHAUSTIVE | inverse_identity=UNKNOWN |
| `mod_add` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | congruent_to_plain_sum=EXHAUSTIVE; commutative=EXHAUSTIVE | congruent_to_plain_sum=UNKNOWN; commutative=PROVED |
| `mod_mul` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | congruent_to_plain_product=EXHAUSTIVE; commutative=EXHAUSTIVE | congruent_to_plain_product=UNKNOWN; commutative=PROVED |
| `is_even` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | period_two=EXHAUSTIVE; alternates=EXHAUSTIVE | period_two=PROVED; alternates=PROVED |
| `pow_mod_naive` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | — | — |
| `mod_pow` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | — | — |
| `pow_mod_agree` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | fast_equals_naive=EXHAUSTIVE | fast_equals_naive=UNKNOWN |
| `isqrt` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / post[1]=SOUND / nat-result (≥ 0)=SOUND | SOUND | SOUND | square_root_of_square=EXHAUSTIVE; floor_bounds=EXHAUSTIVE | square_root_of_square=UNSUPPORTED; floor_bounds=PROVED |
| `mr_squarings` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `mr_witness` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `odd_part_d` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / nat-result (≥ 0)=SOUND | SOUND | SOUND | odd_or_zero=EXHAUSTIVE | odd_or_zero=PROVED |
| `odd_part_s` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / nat-result (≥ 0)=SOUND | SOUND | SOUND | — | — |
| `mr_all` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `is_prime` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `no_divisor_from` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `is_prime_trial` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `prime_agree` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | miller_rabin_agrees_with_trial=EXHAUSTIVE | miller_rabin_agrees_with_trial=UNKNOWN |
| `crt2` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `crt_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | solves_both_congruences=HELD | solves_both_congruences=UNKNOWN |
| `digits` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `from_digits` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | — | — |
| `digits_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | digits_then_from_digits=EXHAUSTIVE | digits_then_from_digits=UNSUPPORTED |
| `digit_sum` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | sum_bounded_by_value=EXHAUSTIVE | sum_bounded_by_value=UNKNOWN |
| `binomial` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | symmetry=EXHAUSTIVE; pascal=EXHAUSTIVE; choose_zero=EXHAUSTIVE | symmetry=UNKNOWN; pascal=UNKNOWN; choose_zero=PROVED |
| `ntt_split` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt_butterfly` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `log2_floor` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=SOUND | SOUND | SOUND | below_power=EXHAUSTIVE | below_power=PROVED |
| `ntt_rec` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `scale_mod` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | length_preserving=HELD | length_preserving=UNSUPPORTED |
| `ntt_inverse` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `pointwise_mod` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | length_is_min=HELD | length_is_min=UNSUPPORTED |
| `pad_to` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | length_is_n=EXHAUSTIVE | length_is_n=UNSUPPORTED |
| `next_pow2` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND / nat-result (≥ 0)=SOUND | SOUND | SOUND | at_least_n=EXHAUSTIVE; under_twice_n=EXHAUSTIVE | at_least_n=PROVED; under_twice_n=PROVED |
| `convolution` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
